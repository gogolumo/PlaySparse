#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import random
import shutil
import statistics
import subprocess
import time
from pathlib import Path
from typing import Iterable

HERE = Path(__file__).resolve().parent
DATASET_DIR = HERE / "dataset"
WORK_DIR = HERE / "work"
RESULTS_DIR = HERE / "results"


def mib(n: int) -> float:
    return n / (1024 * 1024)


def percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    xs = sorted(values)
    if len(xs) == 1:
        return xs[0]
    pos = (len(xs) - 1) * p
    lo = math.floor(pos)
    hi = math.ceil(pos)
    if lo == hi:
        return xs[lo]
    return xs[lo] * (hi - pos) + xs[hi] * (pos - lo)


def zstd_version() -> str:
    out = subprocess.check_output(["zstd", "--version"], text=True, stderr=subprocess.STDOUT)
    return out.strip()


def run(cmd: list[str], *, stdout=None) -> float:
    start = time.perf_counter()
    subprocess.run(cmd, check=True, stdout=stdout, stderr=subprocess.DEVNULL)
    return time.perf_counter() - start


def deterministic_noise(size: int, seed: bytes = b"playsparse-m0") -> bytes:
    # Repeat SHA-256 digests of a deterministic counter. This is intentionally
    # high-entropy-like and reproducible, not cryptographic test material.
    out = bytearray()
    counter = 0
    while len(out) < size:
        out.extend(hashlib.sha256(seed + counter.to_bytes(8, "little")).digest())
        counter += 1
    return bytes(out[:size])


def generate_dataset(path: Path, size_mib: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    total = size_mib * 1024 * 1024
    quarter = total // 4

    text_pattern = (
        b"PlaySparse experimental game storage layer | texture metadata | audio index | "
        b"world chunk | shader cache | manifest | "
    )
    structured = bytes(range(256)) * 4096

    with path.open("wb") as f:
        # 25% highly repetitive text-like data
        remaining = quarter
        while remaining:
            block = text_pattern[: min(len(text_pattern), remaining)]
            f.write(block)
            remaining -= len(block)

        # 25% zeros
        zero = b"\x00" * (1024 * 1024)
        remaining = quarter
        while remaining:
            block = zero[: min(len(zero), remaining)]
            f.write(block)
            remaining -= len(block)

        # 25% deterministic high-entropy-like data
        remaining = quarter
        counter = 0
        while remaining:
            block_size = min(1024 * 1024, remaining)
            f.write(deterministic_noise(block_size, b"playsparse-noise" + counter.to_bytes(4, "little")))
            remaining -= block_size
            counter += 1

        # remainder repeated structured binary pattern
        remaining = total - f.tell()
        while remaining:
            block = structured[: min(len(structured), remaining)]
            f.write(block)
            remaining -= len(block)


def compress_file(src: Path, dst: Path, level: int) -> float:
    dst.parent.mkdir(parents=True, exist_ok=True)
    return run(["zstd", f"-{level}", "-q", "-f", str(src), "-o", str(dst)])


def split_and_compress(src: Path, out_dir: Path, chunk_size: int, level: int) -> tuple[float, list[dict]]:
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)
    manifest: list[dict] = []
    total_time = 0.0
    offset = 0
    index = 0
    with src.open("rb") as f:
        while True:
            data = f.read(chunk_size)
            if not data:
                break
            raw = out_dir / f"{index:06d}.raw"
            comp = out_dir / f"{index:06d}.zst"
            raw.write_bytes(data)
            total_time += compress_file(raw, comp, level)
            raw.unlink()
            manifest.append({
                "index": index,
                "offset": offset,
                "raw_size": len(data),
                "stored_size": comp.stat().st_size,
                "file": comp.name,
            })
            offset += len(data)
            index += 1
    return total_time, manifest


def reconstruct_chunks(chunks_dir: Path, manifest: list[dict], output: Path) -> float:
    start = time.perf_counter()
    with output.open("wb") as dst:
        for entry in manifest:
            subprocess.run(
                ["zstd", "-d", "-q", "-c", str(chunks_dir / entry["file"])],
                check=True,
                stdout=dst,
                stderr=subprocess.DEVNULL,
            )
    return time.perf_counter() - start


def random_read_latencies(
    chunks_dir: Path,
    manifest: list[dict],
    dataset_size: int,
    chunk_size: int,
    read_size: int,
    reads: int,
    seed: int,
) -> list[float]:
    rng = random.Random(seed)
    latencies_ms: list[float] = []
    for _ in range(reads):
        max_start = max(0, dataset_size - read_size)
        offset = rng.randint(0, max_start)
        chunk_index = offset // chunk_size
        within = offset - (chunk_index * chunk_size)
        entry = manifest[chunk_index]
        start = time.perf_counter()
        proc = subprocess.run(
            ["zstd", "-d", "-q", "-c", str(chunks_dir / entry["file"])],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        decoded = proc.stdout
        # Ensure the requested logical range can be served. Reads that cross a
        # chunk boundary are rare at 4 KiB but handled by touching next chunk.
        requested = decoded[within: within + read_size]
        if len(requested) < read_size and chunk_index + 1 < len(manifest):
            proc2 = subprocess.run(
                ["zstd", "-d", "-q", "-c", str(chunks_dir / manifest[chunk_index + 1]["file"])],
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
            )
            requested += proc2.stdout[: read_size - len(requested)]
        if len(requested) != read_size:
            raise RuntimeError("short logical read")
        latencies_ms.append((time.perf_counter() - start) * 1000)
    return latencies_ms


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def write_markdown(result: dict, path: Path) -> None:
    lines = [
        "# Experiment 01 results",
        "",
        f"- Dataset: **{result['dataset']['description']}**",
        f"- Original size: **{result['dataset']['size_mib']:.2f} MiB**",
        f"- Zstd: `{result['environment']['zstd']}`",
        f"- Platform: `{result['environment']['platform']}`",
        f"- Compression level: **{result['config']['level']}**",
        f"- Random read: **{result['config']['read_size']} bytes**, {result['config']['random_reads']} samples",
        "",
        "## Results",
        "",
        "| Layout | Stored MiB | Ratio (orig/stored) | Compress s | Reconstruct MiB/s | Random p50 ms | p95 ms | p99 ms |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    mono = result["monolithic"]
    lines.append(
        f"| monolithic | {mono['stored_mib']:.2f} | {mono['ratio']:.2f}x | {mono['compress_seconds']:.3f} | n/a | n/a | n/a | n/a |"
    )
    for r in result["chunked"]:
        lines.append(
            f"| {r['chunk_label']} | {r['stored_mib']:.2f} | {r['ratio']:.2f}x | {r['compress_seconds']:.3f} | "
            f"{r['reconstruct_mib_s']:.1f} | {r['random_read_ms']['p50']:.3f} | {r['random_read_ms']['p95']:.3f} | {r['random_read_ms']['p99']:.3f} |"
        )
    lines += [
        "",
        "## Interpretation",
        "",
        "This synthetic dataset deliberately mixes compressible and high-entropy regions. It validates the benchmark harness and the chunk-size trade-off; it does **not** predict the compression ratio of GTA, Dota, or any other game.",
        "",
        "The random-read numbers include a full `zstd` process launch per read, so they overstate the latency of a future in-process libzstd implementation. Compare chunk sizes directionally rather than treating the absolute values as a product target.",
        "",
        "Safe-mode integrity check: reconstructed SHA-256 must equal the original SHA-256 for every chunk layout.",
        "",
        f"Original SHA-256: `{result['dataset']['sha256']}`",
    ]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset-mib", type=int, default=64)
    parser.add_argument("--random-reads", type=int, default=120)
    parser.add_argument("--read-size", type=int, default=4096)
    parser.add_argument("--level", type=int, default=3)
    args = parser.parse_args()

    if shutil.which("zstd") is None:
        raise SystemExit("zstd CLI not found in PATH")

    DATASET_DIR.mkdir(parents=True, exist_ok=True)
    WORK_DIR.mkdir(parents=True, exist_ok=True)
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)

    dataset = DATASET_DIR / f"synthetic-{args.dataset_mib}m.bin"
    if not dataset.exists() or dataset.stat().st_size != args.dataset_mib * 1024 * 1024:
        generate_dataset(dataset, args.dataset_mib)

    original_size = dataset.stat().st_size
    original_hash = sha256(dataset)

    mono_path = WORK_DIR / "monolithic.zst"
    mono_time = compress_file(dataset, mono_path, args.level)
    mono_stored = mono_path.stat().st_size

    result = {
        "experiment": "01-zstd-random-access",
        "dataset": {
            "description": "synthetic mixed-entropy baseline (25% repetitive text, 25% zeros, 25% deterministic high-entropy-like bytes, 25% structured repeated binary)",
            "size_bytes": original_size,
            "size_mib": mib(original_size),
            "sha256": original_hash,
            "synthetic": True,
        },
        "environment": {
            "platform": platform.platform(),
            "python": platform.python_version(),
            "zstd": zstd_version(),
        },
        "config": {
            "level": args.level,
            "random_reads": args.random_reads,
            "read_size": args.read_size,
        },
        "monolithic": {
            "stored_bytes": mono_stored,
            "stored_mib": mib(mono_stored),
            "ratio": original_size / mono_stored,
            "compress_seconds": mono_time,
        },
        "chunked": [],
    }

    for chunk_size in [256 * 1024, 1024 * 1024, 4 * 1024 * 1024, 16 * 1024 * 1024]:
        label = f"{chunk_size // 1024} KiB" if chunk_size < 1024 * 1024 else f"{chunk_size // (1024 * 1024)} MiB"
        chunk_dir = WORK_DIR / f"chunks-{chunk_size}"
        compress_seconds, manifest = split_and_compress(dataset, chunk_dir, chunk_size, args.level)
        stored = sum(e["stored_size"] for e in manifest)
        reconstructed = WORK_DIR / f"reconstructed-{chunk_size}.bin"
        reconstruct_seconds = reconstruct_chunks(chunk_dir, manifest, reconstructed)
        reconstructed_hash = sha256(reconstructed)
        if reconstructed_hash != original_hash:
            raise RuntimeError(f"integrity mismatch for {label}")
        lat = random_read_latencies(
            chunk_dir,
            manifest,
            original_size,
            chunk_size,
            args.read_size,
            args.random_reads,
            seed=20261002 + chunk_size,
        )
        result["chunked"].append({
            "chunk_bytes": chunk_size,
            "chunk_label": label,
            "chunks": len(manifest),
            "stored_bytes": stored,
            "stored_mib": mib(stored),
            "ratio": original_size / stored,
            "compress_seconds": compress_seconds,
            "reconstruct_seconds": reconstruct_seconds,
            "reconstruct_mib_s": mib(original_size) / reconstruct_seconds,
            "random_read_ms": {
                "mean": statistics.fmean(lat),
                "p50": percentile(lat, 0.50),
                "p95": percentile(lat, 0.95),
                "p99": percentile(lat, 0.99),
                "min": min(lat),
                "max": max(lat),
            },
            "reconstructed_sha256": reconstructed_hash,
        })

    (RESULTS_DIR / "latest.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    write_markdown(result, RESULTS_DIR / "latest.md")

    print(json.dumps({
        "dataset_mib": result["dataset"]["size_mib"],
        "monolithic_ratio": result["monolithic"]["ratio"],
        "chunked": [
            {
                "chunk": r["chunk_label"],
                "ratio": round(r["ratio"], 3),
                "random_p95_ms": round(r["random_read_ms"]["p95"], 3),
                "reconstruct_mib_s": round(r["reconstruct_mib_s"], 1),
            }
            for r in result["chunked"]
        ],
    }, indent=2))


if __name__ == "__main__":
    main()
