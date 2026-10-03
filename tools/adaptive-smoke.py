#!/usr/bin/env python3
"""Capture genuine mounted traffic, replay STATIC/ADAPTIVE in fresh processes.

The dataset, recorded resolver requests and byte budget are identical for every
replay mode. Preserve all trials, including regressions. OS/device cache state is
uncontrolled; only the PlaySparse chunk cache starts empty in each process.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import random
import subprocess
import sys
import time

spec = importlib.util.spec_from_file_location("mounted_update", Path(__file__).with_name("mounted-update.py"))
updater = importlib.util.module_from_spec(spec)
spec.loader.exec_module(updater)


def workload():
    rng = random.Random(734)
    sequential = [("stream.bin", i * 65536, 65536) for i in range(256)]
    hot_churn = []
    for i in range(64):
        hot_churn.append(("hot.bin", rng.randrange((1 << 20) - 4096), 4096))
        for j in range(8):
            hot_churn.append(("stream.bin", (16 << 20) + ((i * 8 + j) % 256) * 65536, 65536))
    random_reads = [("stream.bin", rng.randrange((32 << 20) - 4096), 4096) for _ in range(128)]
    return sequential + hot_churn + random_reads


def read_queries(root, queries, expected=None):
    digests, latencies = [], []
    streams = {}
    started = time.perf_counter()
    try:
        for name, offset, length in queries:
            if name not in streams:
                streams[name] = (root / name).open("rb", buffering=0)
            stream = streams[name]
            if sys.platform == "linux":
                # File-local advice. This is not global disk cache eviction.
                os.posix_fadvise(stream.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)
            before = time.perf_counter_ns()
            stream.seek(offset)
            data = stream.read(length)
            latencies.append(time.perf_counter_ns() - before)
            if len(data) != length:
                raise RuntimeError("unexpected short mounted read")
            digest = hashlib.sha256(data).hexdigest()
            if expected is not None and digest != expected[len(digests)]:
                raise RuntimeError("mounted bytes differ from original")
            digests.append(digest)
    finally:
        for stream in streams.values():
            stream.close()
    ordered = sorted(latencies)
    return {"queries": len(queries), "requested_bytes": sum(q[2] for q in queries), "aggregate_sha256": hashlib.sha256("".join(digests).encode()).hexdigest(), "wall_seconds": time.perf_counter() - started, "latency_ns": {f"p{p}": ordered[max(0, math.ceil(len(ordered) * p / 100) - 1)] for p in (50, 95, 99)}}, digests


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--playsparse", type=Path, default=Path("target/release/playsparse"))
    parser.add_argument("--io-probe", type=Path, default=Path("target/release/io-probe"))
    parser.add_argument("--repetitions", type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.repetitions <= 10:
        raise ValueError("repetitions must be 1..10")
    cli, probe, work = args.playsparse.resolve(strict=True), args.io_probe.resolve(strict=True), args.work.resolve()
    work.mkdir()
    evidence = work / "evidence"
    evidence.mkdir()
    source, store, mountpoint = (work / n for n in ("source", "base", "mounted"))
    if sys.platform != "win32":
        mountpoint.mkdir()
    repo = Path(__file__).resolve().parent.parent
    git = ["git", "-c", f"safe.directory={repo}", "-C", str(repo)]
    report = {"version": 1, "timestamp": datetime.now(timezone.utc).isoformat(), "commit": subprocess.check_output(git + ["rev-parse", "HEAD"], text=True).strip(), "working_tree_status": subprocess.check_output(git + ["status", "--porcelain"], text=True), "os": platform.platform(), "architecture": platform.machine(), "binary_sha256": hashlib.sha256(cli.read_bytes()).hexdigest(), "status": "FAIL", "commands": [], "cache_bytes": 4 << 20, "cache_state": "fresh PlaySparse cache per replay subprocess; OS/device caches uncontrolled; not disk-cold", "dataset": "synthetic sequential + hot/churn + random; kernel callbacks captured, not a game benchmark"}
    run = lambda command, name: updater.smoke.run_command(command, evidence, name, report["commands"])[0]
    before_source = before_base = None
    try:
        run([sys.executable, str(Path(__file__).with_name("generate-testgame.py")), str(source), "--io-probe", str(probe), "--world-bytes", str(4 << 20)], "generate")
        rng = random.Random(721)
        with (source / "stream.bin").open("wb") as stream:
            for _ in range(512):
                stream.write(rng.randbytes(4096) * 16)
        (source / "hot.bin").write_bytes(random.Random(991).randbytes(1 << 20))
        before_source = updater.smoke.fingerprint(source)
        run([str(cli), "pack", str(source), str(store), "--chunker", "fixed", "--chunk-size", "64K"], "pack")
        before_base = updater.smoke.fingerprint(store)
        queries = workload()
        report["original"], expected = read_queries(source, queries)
        trace = evidence / "capture.trace.jsonl"
        capture = updater.Mount(cli, store, mountpoint, None, trace, evidence, "capture", report["commands"], cache="4M")
        with capture:
            report["capture_mount_record"] = capture.record
            report["mounted_static"], _ = read_queries(mountpoint, queries, expected)
        report["capture_metrics"] = capture.events
        policy = evidence / "policy.json"
        report["policy"] = run([str(cli), "optimize", str(trace), "--output", str(policy)], "optimize")
        # Compare the persisted trace-derived policy without adjusting it after
        # seeing benchmark results. Every trial uses exactly the same policy.
        evaluation = report["policy"]
        evaluation_policy = policy
        report["evaluation_policy"] = evaluation
        trials = {"static": [], "adaptive": []}
        for trial in range(args.repetitions):
            for mode in (("static", "adaptive") if trial % 2 == 0 else ("adaptive", "static")):
                command = [str(cli), "replay-one", str(store), str(trace), "--cache", "4M", "--output", str(evidence / f"{mode}-{trial}.json")]
                if mode == "adaptive":
                    command.extend(["--policy", str(evaluation_policy)])
                trials[mode].append(run(command, f"{mode}-{trial}"))
        report["replay_trials"] = trials
        hashes = {row["content_blake3"] for rows in trials.values() for row in rows}
        if len(hashes) != 1:
            raise RuntimeError("static/adaptive replay bytes differ")
        adaptive = updater.Mount(cli, store, mountpoint, None, evidence / "adaptive.trace.jsonl", evidence, "adaptive", report["commands"], extra=["--policy", str(evaluation_policy)], cache="4M")
        with adaptive:
            report["adaptive_mount_record"] = adaptive.record
            report["mounted_adaptive"], _ = read_queries(mountpoint, queries, expected)
        report["adaptive_metrics"] = adaptive.events
        report["trace_summary"] = run([str(cli), "trace", "summarize", str(trace)], "trace-summary")
        report["status"] = "PASS"
    except Exception as exc:
        report["error"] = str(exc)
    finally:
        for name, root, before in (("source", source, before_source), ("base", store, before_base)):
            if before:
                report[f"{name}_unchanged"] = before["tree_fingerprint_sha256"] == updater.smoke.fingerprint(root)["tree_fingerprint_sha256"]
                if not report[f"{name}_unchanged"]:
                    report["status"] = "FAIL"
        (evidence / "result.json").write_text(json.dumps(report, indent=2))
        print(json.dumps({"status": report["status"], "evidence": str(evidence), "error": report.get("error")}, indent=2))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
