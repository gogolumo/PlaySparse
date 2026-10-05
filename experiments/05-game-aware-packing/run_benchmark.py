#!/usr/bin/env python3
"""L0 exact-input offline packing/reuse frontier. Keep every trial and failure."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import random
import statistics
import subprocess
import sys
import zipfile

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools"))
from validation_common import (CommandRunner, atomic_json, digest, repository_identity,
                               safe_new_work, signals)


def tree_identity(root):
    rows = []
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("fixture symlink")
        rows.append({"path": path.relative_to(root).as_posix(), "size": path.stat().st_size if path.is_file() else None,
                     "sha256": digest(path) if path.is_file() else None,
                     "mode": path.stat().st_mode & 0o777})
    return {"files": rows, "sha256": hashlib.sha256(json.dumps(rows, sort_keys=True).encode()).hexdigest()}


def archive(path, entries):
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as stream:
        for name, raw in entries:
            item = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
            item.compress_type = zipfile.ZIP_DEFLATED
            stream.writestr(item, raw)


def generate(work, size_mib, adverse=False):
    """Stable incompressible entry records, compressible scripts and a shifted update."""
    rng = random.Random(505)
    roots = [work / "v1", work / "v2"]
    for root in roots:
        root.mkdir()
    unit = 1 << 20
    entries = [(f"assets/entry-{i:03}.bin", rng.randbytes(unit)) for i in range(size_mib)]
    updated = list(entries)
    updated.insert(1, ("assets/new-record.bin", rng.randbytes(unit // 3)))
    name, raw = updated[-2]
    updated[-2] = (name, raw[:unit // 2] + rng.randbytes(8192) + raw[unit // 2 + 8192:])
    for root, version in zip(roots, [entries, updated]):
        archive(root / "content.pk3", version)
    raw = rng.randbytes(size_mib * unit // 2)
    scripts = b"-- generated, redistributable PlaySparse fixture\nlocal item = {name='same', value=42}\n" * 8192
    for root in roots:
        (root / "raw.dat").write_bytes(raw)
        (root / "script.lua").write_bytes(scripts)
        if adverse:
            # Deliberately adversarial to bounded sampling: only the probe regions
            # are high entropy. The loss must remain visible, never tune it away.
            length = size_mib * unit
            misleading = bytearray(length)
            for offset in (0, (length - 65536) // 2, length - 65536):
                misleading[offset:offset + 65536] = rng.randbytes(65536)
            (root / "misleading.pak").write_bytes(misleading)
    return roots


def update_reuse(v1, v2):
    def index(store):
        b = (store / "index/objects.idx").read_bytes()
        if b[:8] != b"PSPIDX01" or len(b) != 16 + 56 * int.from_bytes(b[8:16], "little"):
            raise ValueError("unexpected store index")
        return {b[p:p + 32].hex(): {"encoded": int.from_bytes(b[p + 44:p + 48], "little"),
                                  "raw": int.from_bytes(b[p + 48:p + 52], "little")}
                for p in range(16, len(b), 56)}
    old, new = index(v1), index(v2)
    manifest = json.loads((v2 / "manifest.json").read_text())
    chunks = [c for f in manifest["files"] for c in f["chunks"]]
    shared = set(old).intersection(new)
    added = set(new) - set(old)
    metadata = sum((v2 / p).stat().st_size for p in ("manifest.json", "index/objects.idx", "COMMITTED.json"))
    return {"reused_v2_chunks": sum(c["hash"] in old for c in chunks), "v2_chunks": len(chunks),
            "cross_version_reused_logical_bytes": sum(c["raw_size"] for c in chunks if c["hash"] in old),
            "cross_version_reused_unique_raw_bytes": sum(new[h]["raw"] for h in shared),
            "new_unique_objects": len(added), "new_encoded_object_bytes": sum(new[h]["encoded"] for h in added),
            "new_metadata_bytes": metadata,
            "new_physical_bytes_if_existing_objects_reused": sum(new[h]["encoded"] for h in added) + metadata + 8,
            "definition": "hypothetical shared-object update: missing v2 payloads + complete v2 metadata + one pack header; stores are independently published, no delta-update API or allocation claim"}


def stats(values):
    present = [v for v in values if v is not None]
    if len(present) != len(values) or not present:
        return None
    return {"median": statistics.median(present), "variance": statistics.pvariance(present),
            "min": min(present), "max": max(present), "trials": len(present)}


def summarize(runs):
    metrics = {
        "physical_bytes": lambda r: r["pack"]["physical_bytes"],
        "object_bytes": lambda r: r["pack"]["object_bytes"],
        "metadata_bytes": lambda r: r["pack"]["metadata_bytes"],
        "allocated_bytes": lambda r: r["pack"]["allocated_bytes"],
        "pack_wall_seconds": lambda r: r["pack_command"]["wall_seconds"],
        "pack_cpu_seconds": lambda r: r["pack_cpu_seconds"],
        "inspect_plan_wall_seconds": lambda r: r["inspect_wall_seconds"],
        "inspect_plan_cpu_seconds": lambda r: r["inspect_cpu_seconds"],
        "end_to_end_wall_seconds": lambda r: r["pack_command"]["wall_seconds"] + r["inspect_wall_seconds"],
        "end_to_end_cpu_seconds": lambda r: None if r["pack_cpu_seconds"] is None or r["inspect_cpu_seconds"] is None else r["pack_cpu_seconds"] + r["inspect_cpu_seconds"],
        "unique_objects": lambda r: r["pack"]["unique_objects"],
        "reused_chunks_within_v2": lambda r: r["pack"]["reused_chunks"],
        "update_new_physical_bytes": lambda r: r["update"]["new_physical_bytes_if_existing_objects_reused"],
        "cross_version_reused_bytes": lambda r: r["update"]["cross_version_reused_logical_bytes"],
        "cold_p50_ms": lambda r: r["benchmark"]["workloads"][0]["playsparse_chunk_cache_cold"]["p50_ms"],
        "cold_p95_ms": lambda r: r["benchmark"]["workloads"][0]["playsparse_chunk_cache_cold"]["p95_ms"],
        "cold_p99_ms": lambda r: r["benchmark"]["workloads"][0]["playsparse_chunk_cache_cold"]["p99_ms"],
        "warm_p50_ms": lambda r: r["benchmark"]["workloads"][0]["playsparse_warm_replay"]["p50_ms"],
        "warm_p95_ms": lambda r: r["benchmark"]["workloads"][0]["playsparse_warm_replay"]["p95_ms"],
        "warm_p99_ms": lambda r: r["benchmark"]["workloads"][0]["playsparse_warm_replay"]["p99_ms"],
        "sequential_mib_s": lambda r: r["benchmark"]["sequential"]["playsparse_mib_s"],
        "read_benchmark_cpu_seconds": lambda r: r["benchmark"]["process_cpu_seconds"],
        "peak_rss_bytes": lambda r: r["benchmark"]["peak_rss_bytes"],
        "cold_raw_read_amplification_4k": lambda r: r["benchmark"]["workloads"][0]["read_amplification"],
        "warm_cache_hit_ratio": lambda r: r["benchmark"]["warm_cache"]["hits"] / max(1, r["benchmark"]["warm_cache"]["hits"] + r["benchmark"]["warm_cache"]["misses"]),
    }
    return {mode: {name: stats([get(r) for r in runs if r["mode"] == mode]) for name, get in metrics.items()}
            for mode in dict.fromkeys(r["mode"] for r in runs)}


def cpu_children():
    try:
        import resource
        r = resource.getrusage(resource.RUSAGE_CHILDREN)
        return r.ru_utime + r.ru_stime
    except (ImportError, OSError):
        return None


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--work", type=Path, required=True)
    p.add_argument("--playsparse", type=Path, default=REPO / "target/release/playsparse")
    p.add_argument("--baseline", type=Path, help="preserved current-main binary; defaults to same binary's generic path")
    p.add_argument("--baseline-commit", default=None)
    p.add_argument("--size-mib", type=int, default=16)
    p.add_argument("--repetitions", type=int, default=3)
    p.add_argument("--iterations", type=int, default=200)
    p.add_argument("--adversarial", action="store_true")
    args = p.parse_args()
    if not 1 <= args.size_mib <= 128 or not 2 <= args.repetitions <= 10 or not 1 <= args.iterations <= 10000:
        p.error("bounded size 1..128 MiB, repetitions 2..10, iterations 1..10000")
    binary = args.playsparse.resolve(strict=True)
    baseline = args.baseline.resolve(strict=True) if args.baseline else binary
    work = safe_new_work(args.work, repo=REPO)
    evidence = work / "evidence"
    evidence.mkdir()
    report = {"version": 1, "experiment": "05-game-aware-packing", "evidence_level": "L0",
              "timestamp": datetime.now(timezone.utc).isoformat(), "platform": platform.platform(),
              "repository": repository_identity(REPO), "binary_sha256": digest(binary),
              "baseline_binary_sha256": digest(baseline), "baseline_commit": args.baseline_commit,
              "repetitions": args.repetitions, "iterations": args.iterations,
              "adversarial_probe_regions": args.adversarial, "runs": [], "status": "RUNNING",
              "limits": {"OS_device_cache": "uncontrolled; cold means disabled decompressed cache",
                         "read_cpu": "whole direct benchmark including original file reads, correctness hashing and store opens",
                         "startup": "NOT RUN", "native_compression": "NOT RUN", "mounted_performance": "NOT RUN",
                         "physical_device_amplification": None, "runtime_policy": "unchanged static LRU; no prefetch",
                         "L1": "NOT RUN", "L2": "NOT RUN"}}
    runner = CommandRunner(REPO, evidence, report, timeout=600)
    sources = []
    before = None
    def command(argv, label):
        cpu = cpu_children()
        row = runner.run(argv, label)
        end = cpu_children()
        payload = json.loads((evidence / f"{label}.stdout.log").read_text())
        atomic_json(evidence / f"{label}.json", payload)
        return payload, row, None if cpu is None or end is None else end - cpu
    try:
        with signals():
            sources = generate(work, args.size_mib, args.adversarial)
            before = [tree_identity(s) for s in sources]
            report["sources_before"] = before
            for rep in range(args.repetitions):
                modes = ["generic-cdc", "measured-codec", "zip-records", "generic-fixed"]
                modes = modes[rep % 4:] + modes[:rep % 4]
                for mode in modes:
                    stores = []
                    for version, source in enumerate(sources, 1):
                        label = f"r{rep}-{mode}-v{version}"
                        store = work / label
                        chosen = baseline if mode.startswith("generic") else binary
                        argv = [str(chosen), "pack", str(source), str(store)]
                        inspection = None
                        inspect_wall, inspect_cpu = 0.0, 0.0
                        if mode.startswith("generic"):
                            argv += ["--chunker", "fixed" if mode == "generic-fixed" else "cdc"]
                        else:
                            profile = evidence / f"{label}-profile.json"
                            plan = evidence / f"{label}-plan.json"
                            inspect_argv = [str(binary), "inspect-game", str(source), "--scanner", "generic", "--output", str(profile), "--plan-output", str(plan)]
                            if mode == "zip-records":
                                inspect_argv.append("--container-aware")
                            if mode == "measured-codec":
                                inspect_argv.append("--experimental-skip-compression")
                            inspection, inspect_row, inspect_cpu = command(inspect_argv, label + "-inspect")
                            inspect_wall = inspect_row["wall_seconds"]
                            argv += ["--plan", str(plan)]
                        packed, pack_row, cpu = command(argv, label + "-pack")
                        verified, _, _ = command([str(baseline), "verify", str(store)], label + "-old-reader-verify")
                        if not verified.get("ok"):
                            raise ValueError("v1 old reader verification failed")
                        stores.append(store)
                        if version == 2:
                            bench, _, _ = command([str(binary), "benchmark", str(source), str(store), "--iterations", str(args.iterations), "--cache", "16M"], label + "-benchmark")
                            report["runs"].append({"mode": mode, "repetition": rep, "pack": packed,
                                                   "pack_command": pack_row, "pack_cpu_seconds": cpu,
                                                   "inspect_wall_seconds": inspect_wall,
                                                   "inspect_cpu_seconds": inspect_cpu,
                                                   "inspection": inspection, "benchmark": bench,
                                                   "update": update_reuse(stores[0], stores[1])})
                    if [tree_identity(s) for s in sources] != before:
                        raise ValueError("source changed during experiment")
            # Query generator must have stayed identical for all packing configurations.
            identities = [[w["aggregate_read_blake3"] for w in r["benchmark"]["workloads"]] for r in report["runs"]]
            if any(value != identities[0] for value in identities):
                raise ValueError("benchmark input/query byte identity mismatch")
            report["query_bytes_equal"] = True
            report["summary"] = summarize(report["runs"])
            if repository_identity(REPO) != report["repository"] or digest(binary) != report["binary_sha256"] or digest(baseline) != report["baseline_binary_sha256"]:
                raise ValueError("repository or binary changed during experiment")
            report["status"] = "PASS"
    except BaseException as error:
        report.update(status="FAIL", error=str(error), failure_type=type(error).__name__)
    finally:
        report["sources_after"] = [tree_identity(s) for s in sources]
        report["source_unchanged"] = before is not None and report["sources_after"] == before
        if not report["source_unchanged"]:
            report["status"] = "FAIL"
        atomic_json(evidence / "result.json", report)
    print(json.dumps({"status": report["status"], "evidence": str(evidence), "error": report.get("error"), "summary": report.get("summary")}, indent=2))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
