#!/usr/bin/env python3
"""Measure verified original and real mounted reads on a Linux FUSE host."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import subprocess
import sys
import tempfile
import time


def percentiles(values):
    ordered = sorted(values)
    return {"samples": len(ordered), **{
        f"p{p}_ms": ordered[max(0, math.ceil(len(ordered) * p / 100) - 1)] / 1e6
        for p in (50, 95, 99)
    }}


def allocation(root):
    encoded = allocated = entries = 0
    for directory, _, files in os.walk(root):
        stat = os.stat(directory)
        allocated += stat.st_blocks * 512
        entries += 1
        for name in files:
            stat = os.stat(Path(directory) / name)
            encoded += stat.st_size
            allocated += stat.st_blocks * 512
            entries += 1
    return {"encoded_bytes": encoded, "allocated_bytes": allocated, "filesystem_entries": entries}


def provider_usage(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    cpu = (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    status = Path(f"/proc/{pid}/status").read_text().splitlines()
    peak = next(int(line.split()[1]) * 1024 for line in status if line.startswith("VmHWM:"))
    return cpu, peak


def measure(root, queries, *, evict, expected=None, provider=None):
    fds, latencies, opens, digests = {}, [], [], []
    cpu_before = provider_usage(provider)[0] if provider else None
    client_cpu = time.process_time()
    started = time.perf_counter()
    try:
        for index, (name, offset, length) in enumerate(queries):
            if name not in fds:
                opened = time.perf_counter_ns()
                fds[name] = os.open(root / name, os.O_RDONLY)
                opens.append(time.perf_counter_ns() - opened)
            fd = fds[name]
            if evict:
                # File-local advice only: no global cache dropping. The OS may
                # retain busy pages; underlying device caches are uncontrolled.
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            before = time.perf_counter_ns()
            data = os.pread(fd, length, offset)
            latencies.append(time.perf_counter_ns() - before)
            if len(data) != length:
                raise RuntimeError(f"short read: {name}:{offset}: {len(data)} != {length}")
            digest = hashlib.sha256(data).hexdigest()
            if expected is not None and digest != expected[index]:
                raise RuntimeError(f"byte digest differs: {name}:{offset}")
            digests.append(digest)
    finally:
        for fd in fds.values():
            os.close(fd)
    wall = time.perf_counter() - started
    cpu_after, peak = provider_usage(provider) if provider else (None, None)
    cpu_delta = cpu_after - cpu_before if provider else None
    result = {
        "read_latency": percentiles(latencies), "open_latency": percentiles(opens),
        "requested_bytes": sum(query[2] for query in queries),
        "wall_seconds": wall, "client_cpu_seconds": time.process_time() - client_cpu,
        "provider_cpu_seconds": cpu_delta,
        "provider_cpu_percent_one_core": 100 * cpu_delta / wall if provider else None,
        "provider_peak_rss_bytes": peak,
        "aggregate_query_sha256": hashlib.sha256("".join(digests).encode()).hexdigest(),
        "verified_reads": len(queries),
    }
    return result, digests


class Mount:
    def __init__(self, cli, store, cache):
        self.cli, self.store, self.cache = cli, store, cache

    def __enter__(self):
        self.temp = tempfile.TemporaryDirectory(prefix="playsparse-frontier-")
        self.path = Path(self.temp.name) / "mount"
        self.path.mkdir()
        self.log_path = Path(self.temp.name) / "provider.stderr"
        self.log = self.log_path.open("w")
        self.process = subprocess.Popen(
            [str(self.cli), "mount", str(self.store), str(self.path), "--cache", self.cache],
            stdout=subprocess.DEVNULL, stderr=self.log,
        )
        deadline = time.monotonic() + 20
        while not os.path.ismount(self.path):
            if self.process.poll() is not None:
                self.log.close()
                raise RuntimeError(f"mount failed: {self.log_path.read_text()}")
            if time.monotonic() > deadline:
                self.process.terminate()
                self.process.wait(timeout=5)
                raise RuntimeError("real FUSE mount did not become ready")
            time.sleep(0.02)
        self.real_mount_verified = True
        return self

    def __exit__(self, kind, value, traceback):
        try:
            if os.path.ismount(self.path):
                subprocess.run([str(self.cli), "unmount", str(self.path)], check=True,
                               capture_output=True, timeout=20)
            self.process.wait(timeout=20)
            self.log.close()
            self.stderr = self.log_path.read_text()
            events = []
            for line in self.stderr.splitlines():
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if event.get("event") == "fuse_unmounted":
                    events.append(event)
            if self.process.returncode != 0 or not events:
                raise RuntimeError(f"provider failed or metrics missing: {self.stderr}")
            self.metrics = events[-1]
            if self.metrics["read_errors"]:
                raise RuntimeError(f"provider reported read errors: {self.metrics}")
            if os.path.ismount(self.path) or list(self.path.iterdir()):
                raise RuntimeError("mount did not detach cleanly or materialized files appeared")
        finally:
            self.temp.cleanup()


def sequential(root, files, *, provider=None, expected=None):
    digest = hashlib.sha256()
    total = 0
    cpu_before = provider_usage(provider)[0] if provider else None
    started = time.perf_counter()
    for entry in files:
        fd = os.open(root / entry["path"], os.O_RDONLY)
        file_bytes = 0
        try:
            os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            while True:
                data = os.read(fd, 1024 * 1024)
                if not data:
                    break
                total += len(data)
                file_bytes += len(data)
                digest.update(data)
        finally:
            os.close(fd)
        if file_bytes != entry["size"]:
            raise RuntimeError(f"sequential file size differs: {entry['path']}")
    seconds = time.perf_counter() - started
    if expected and digest.hexdigest() != expected:
        raise RuntimeError("sequential original/mounted digest differs")
    cpu_after, peak = provider_usage(provider) if provider else (None, None)
    return {"bytes": total, "seconds": seconds, "mib_s": total / 1048576 / seconds,
            "sha256": digest.hexdigest(), "provider_peak_rss_bytes": peak,
            "provider_cpu_seconds": cpu_after - cpu_before if provider else None,
            "includes": "file opens, sequential reads and SHA-256 verification"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("store", type=Path)
    parser.add_argument("--cli", type=Path, default=Path("target-linux/release/playsparse"))
    parser.add_argument("--reads", type=int, default=200)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if platform.system() != "Linux" or not hasattr(os, "posix_fadvise"):
        raise SystemExit("requires real Linux FUSE and POSIX file-local cache advice")
    if not 1 <= args.reads <= 10000 or not 1 <= args.runs <= 10:
        raise SystemExit("reads must be 1..10000 and runs 1..10")
    source, store, cli = args.source.resolve(), args.store.resolve(), args.cli.resolve()
    manifest = json.loads((store / "manifest.json").read_text())
    source_before, store_before = allocation(source), allocation(store)
    manifest_digest = hashlib.sha256((store / "manifest.json").read_bytes()).hexdigest()
    repo = Path(__file__).resolve().parent.parent
    git = ["git", "-c", f"safe.directory={repo}", "-C", str(repo)]
    output = {
        "kind": "actual mounted OS file API frontier", "os": platform.platform(),
        "commit": subprocess.check_output(git + ["rev-parse", "HEAD"], text=True).strip(),
        "working_tree_status": subprocess.check_output(git + ["status", "--porcelain"], text=True),
        "command": [sys.executable, *sys.argv],
        "cli_sha256": hashlib.sha256(cli.read_bytes()).hexdigest(),
        "machine": platform.machine(), "cpu_count": os.cpu_count(),
        "source": str(source), "store": str(store), "cli": str(cli),
        "source_storage": source_before, "store_storage": store_before,
        "savings_encoded_percent": 100 * (1 - store_before["encoded_bytes"] / source_before["encoded_bytes"]),
        "manifest_sha256": manifest_digest,
        "cold_definition": "fresh FUSE mount, userspace chunk cache disabled, POSIX_FADV_DONTNEED on the requested virtual file before each read; native baseline uses the same file-local advice; advice may retain busy pages; compressed pack OS cache and device caches uncontrolled; NOT disk-cold",
        "warm_definition": "fresh FUSE mount with 256M chunk cache; one untimed preload of identical queries, then timed replay with kernel page cache retained; native baseline uses identical preload/replay",
        "latency_definition": "pread only; open, file-local cache advice and SHA-256 verification excluded from latency; CPU/wall includes the whole measurement loop",
        "provider_cpu_definition": "/proc/PID/stat utime+stime delta; percentage of one CPU core, 10ms tick granularity; excludes client and kernel worker CPU",
        "memory_definition": "provider VmHWM; excludes kernel page cache and benchmark client memory",
        "runs": [],
    }
    for run in range(args.runs):
        workloads = []
        for length in (4096, 65536, 1048576):
            eligible = [entry for entry in manifest["files"] if entry["size"] >= length]
            if not eligible:
                raise RuntimeError(f"no files can serve full {length}-byte queries")
            rng = random.Random(0x504C4159 + run * 100 + length)
            queries = []
            for _ in range(args.reads):
                file = rng.choice(eligible)
                queries.append((file["path"], rng.randrange(file["size"] - length + 1), length))
            native_cold, expected = measure(source, queries, evict=True)
            measure(source, queries, evict=False, expected=expected)
            native_warm, _ = measure(source, queries, evict=False, expected=expected)
            with Mount(cli, store, "0") as cold_mount:
                mounted_cold, _ = measure(cold_mount.path, queries, evict=True,
                                           expected=expected, provider=cold_mount.process.pid)
            with Mount(cli, store, "256M") as warm_mount:
                measure(warm_mount.path, queries, evict=False, expected=expected,
                        provider=warm_mount.process.pid)
                mounted_warm, _ = measure(warm_mount.path, queries, evict=False,
                                           expected=expected, provider=warm_mount.process.pid)
            cold_metrics, warm_metrics = cold_mount.metrics, warm_mount.metrics
            workloads.append({
                "read_bytes": length, "eligible_files": len(eligible),
                "original_cold": native_cold, "original_warm": native_warm,
                "mounted_cold": mounted_cold, "mounted_warm": mounted_warm,
                "real_mount_verified": cold_mount.real_mount_verified and warm_mount.real_mount_verified,
                "cold_provider_metrics": cold_metrics, "warm_provider_metrics": warm_metrics,
                "cold_read_amplification": cold_metrics["cache"]["raw_bytes_loaded"] / mounted_cold["requested_bytes"],
                "warm_read_amplification_including_preload": warm_metrics["cache"]["raw_bytes_loaded"] / (2 * mounted_warm["requested_bytes"]),
                "warm_metrics_window": "includes untimed preload plus measured replay; kernel cache hits do not reach the provider",
            })
        output["runs"].append({"run": run + 1, "workloads": workloads})
        print(f"verified mounted frontier run {run + 1}/{args.runs}", flush=True)
    native_sequence = sequential(source, manifest["files"])
    with Mount(cli, store, "0") as sequence_mount:
        mounted_sequence = sequential(sequence_mount.path, manifest["files"],
                                      provider=sequence_mount.process.pid, expected=native_sequence["sha256"])
    output["sequential"] = {"original": native_sequence, "mounted": mounted_sequence,
                             "provider_metrics": sequence_mount.metrics,
                             "bytes_equal": True, "real_mount_verified": True}
    if allocation(source) != source_before or allocation(store) != store_before:
        raise RuntimeError("fixture/store footprint changed")
    if hashlib.sha256((store / "manifest.json").read_bytes()).hexdigest() != manifest_digest:
        raise RuntimeError("store manifest changed")
    output["footprints_unchanged"] = True
    output["no_full_pre_extraction"] = "real FUSE mount served CAS callbacks; each detached mountpoint empty; source/store footprints unchanged"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n")
    print(f"saved {args.output}", flush=True)


if __name__ == "__main__":
    main()
