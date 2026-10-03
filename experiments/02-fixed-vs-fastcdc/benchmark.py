#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import random
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from playsparse_lab.chunking import fixed_chunks, fastcdc_chunks

HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"


def deterministic_noise(size: int, seed: int) -> bytes:
    rng = random.Random(seed)
    return rng.randbytes(size)


def build_base(size_mib: int) -> bytes:
    target = size_mib * 1024 * 1024
    out = bytearray()
    dictionary = [
        (f"asset/{i:04d}/metadata|mesh|texture|shader|audio|".encode() * 2048)[:64 * 1024]
        for i in range(32)
    ]
    block_index = 0
    while len(out) < target:
        mode = block_index % 5
        if mode in (0, 1):
            block = dictionary[(block_index * 7) % len(dictionary)]
        elif mode == 2:
            block = bytes([block_index % 251]) * (64 * 1024)
        elif mode == 3:
            block = bytes((i + block_index) % 256 for i in range(64 * 1024))
        else:
            block = deterministic_noise(64 * 1024, 1000 + block_index)
        out.extend(block)
        block_index += 1
    return bytes(out[:target])


def build_patch(base: bytes) -> bytes:
    insert_at = len(base) // 3 + 777
    insert = (b"NEW_LEVEL_PATCH_DATA|" * 7000)[:128 * 1024]
    patched = bytearray(base[:insert_at] + insert + base[insert_at:])
    replace_at = (len(patched) * 3) // 4
    replacement = deterministic_noise(96 * 1024, 424242)
    patched[replace_at:replace_at + len(replacement)] = replacement
    return bytes(patched)


def chunk_digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def zstd_size(data: bytes, level: int) -> int:
    proc = subprocess.run(
        ["zstd", f"-{level}", "-q", "-c"],
        input=data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return len(proc.stdout)


def evaluate(name: str, base: bytes, updated: bytes, chunker, level: int) -> dict:
    t0 = time.perf_counter()
    base_chunks = list(chunker(base))
    t1 = time.perf_counter()
    updated_chunks = list(chunker(updated))
    t2 = time.perf_counter()

    base_map = {chunk_digest(c.data): c.size for c in base_chunks}
    updated_ids = [(chunk_digest(c.data), c.size, c.data) for c in updated_chunks]
    reused_v2_bytes = sum(size for digest, size, _ in updated_ids if digest in base_map)
    reused_v2_chunks = sum(1 for digest, _, _ in updated_ids if digest in base_map)

    unique: dict[str, bytes] = {}
    for c in base_chunks:
        unique.setdefault(chunk_digest(c.data), c.data)
    for digest, _, data in updated_ids:
        unique.setdefault(digest, data)

    comp_start = time.perf_counter()
    compressed_unique = sum(zstd_size(data, level) for data in unique.values())
    comp_seconds = time.perf_counter() - comp_start

    logical_two_versions = len(base) + len(updated)
    raw_unique = sum(len(v) for v in unique.values())
    manifest = json.dumps({
        "base": [{"h": chunk_digest(c.data), "o": c.offset, "s": c.size} for c in base_chunks],
        "updated": [{"h": digest, "o": c.offset, "s": size} for c, (digest, size, _) in zip(updated_chunks, updated_ids)],
    }, separators=(",", ":")).encode()

    return {
        "name": name,
        "base_chunks": len(base_chunks),
        "updated_chunks": len(updated_chunks),
        "base_avg_chunk": len(base) / max(1, len(base_chunks)),
        "updated_avg_chunk": len(updated) / max(1, len(updated_chunks)),
        "chunking_seconds_base": t1 - t0,
        "chunking_seconds_updated": t2 - t1,
        "reused_v2_chunks": reused_v2_chunks,
        "reused_v2_bytes": reused_v2_bytes,
        "v2_reuse_ratio": reused_v2_bytes / len(updated),
        "two_version_logical_bytes": logical_two_versions,
        "unique_raw_bytes": raw_unique,
        "unique_raw_ratio": raw_unique / logical_two_versions,
        "compressed_unique_bytes": compressed_unique,
        "compressed_unique_ratio": compressed_unique / logical_two_versions,
        "compression_seconds": comp_seconds,
        "manifest_bytes": len(manifest),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset-mib", type=int, default=24)
    parser.add_argument("--chunk-kib", type=int, default=256)
    parser.add_argument("--level", type=int, default=3)
    parser.add_argument("--output", type=Path, default=RESULTS)
    args = parser.parse_args()
    results = args.output
    if shutil.which("zstd") is None:
        raise SystemExit("zstd CLI is required")

    base = build_base(args.dataset_mib)
    updated = build_patch(base)
    avg = args.chunk_kib * 1024

    fixed = evaluate("fixed", base, updated, lambda data: fixed_chunks(data, avg), args.level)
    cdc = evaluate(
        "fastcdc-style",
        base,
        updated,
        lambda data: fastcdc_chunks(data, min_size=avg // 4, avg_size=avg, max_size=avg * 4),
        args.level,
    )

    result = {
        "experiment": "02-fixed-vs-fastcdc",
        "dataset": {
            "synthetic": True,
            "base_bytes": len(base),
            "updated_bytes": len(updated),
            "update": "128 KiB insertion near 1/3 + 96 KiB replacement near 3/4",
        },
        "config": {"target_chunk_kib": args.chunk_kib, "zstd_level": args.level},
        "results": [fixed, cdc],
        "conclusion": {
            "cdc_patch_reuse_delta_percentage_points": (cdc["v2_reuse_ratio"] - fixed["v2_reuse_ratio"]) * 100,
            "cdc_compressed_store_delta_percent": ((cdc["compressed_unique_bytes"] / fixed["compressed_unique_bytes"]) - 1) * 100,
        },
    }
    results.mkdir(parents=True, exist_ok=True)
    (results / "latest.json").write_text(json.dumps(result, indent=2) + "\n")
    md = [
        "# Experiment 02 results",
        "",
        "> Synthetic patch-resilience benchmark; this is not a commercial-game result.",
        "",
        f"Base: **{len(base)/(1024*1024):.2f} MiB**; updated: **{len(updated)/(1024*1024):.2f} MiB**.",
        "Update: 128 KiB insertion plus 96 KiB replacement.",
        "",
        "| Chunker | v2 bytes reused from v1 | Unique raw / two versions | Compressed unique / two versions | Chunking v1+v2 | Manifest |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for r in result["results"]:
        md.append(
            f"| {r['name']} | {r['v2_reuse_ratio']*100:.2f}% | {r['unique_raw_ratio']*100:.2f}% | "
            f"{r['compressed_unique_ratio']*100:.2f}% | {(r['chunking_seconds_base']+r['chunking_seconds_updated']):.3f}s | {r['manifest_bytes']/1024:.1f} KiB |"
        )
    md += [
        "",
        "## Interpretation",
        "",
        f"CDC changed update reuse by **{result['conclusion']['cdc_patch_reuse_delta_percentage_points']:+.2f} percentage points** versus fixed offsets.",
        f"Its compressed two-version store changed by **{result['conclusion']['cdc_compressed_store_delta_percent']:+.2f}%** versus fixed chunks.",
        "",
        "This experiment tests one thing: whether content-defined boundaries recover after inserted data and therefore improve version reuse. It does not establish that CDC saves meaningful space inside a single already-compressed AAA install.",
        "",
        "The implementation is a small FastCDC-style normalized Gear reference. Production code should use a vetted Rust FastCDC implementation and repeat this benchmark on open game-like corpora.",
    ]
    (results / "latest.md").write_text("\n".join(md) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
