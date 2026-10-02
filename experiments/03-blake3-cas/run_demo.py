#!/usr/bin/env python3
from __future__ import annotations

import json
from pathlib import Path
import random
import shutil
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from playsparse_lab.cas import pack_directory, verify_store, unpack_store, tree_sha256

HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"


def noise(size: int, seed: int) -> bytes:
    return random.Random(seed).randbytes(size)


def make_corpus(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    common = (b"PLAYSPARSE_COMMON_ASSET|" * 4000)[:64 * 1024]
    structured = bytes(range(256)) * 512
    game_a = common * 4 + structured * 2 + noise(256 * 1024, 1)
    game_b = common * 4 + structured * 2 + noise(256 * 1024, 2)
    archive_v1 = (b"WORLD_REGION|" * 12000) + noise(512 * 1024, 3)
    insert = (b"PATCH_INSERT|" * 2500)[:32 * 1024]
    archive_v2 = archive_v1[:180_123] + insert + archive_v1[180_123:]
    (root / "game-a.bin").write_bytes(game_a)
    (root / "game-b.bin").write_bytes(game_b)
    (root / "archive-v1.pak").write_bytes(archive_v1)
    (root / "archive-v2.pak").write_bytes(archive_v2)
    (root / "readme.txt").write_text("Generated PlaySparse CAS corpus.\n" * 200)


def main() -> None:
    if shutil.which("zstd") is None:
        raise SystemExit("zstd CLI required")
    with tempfile.TemporaryDirectory(prefix="playsparse-cas-") as td:
        tmp = Path(td)
        src = tmp / "source"
        store = tmp / "store"
        out = tmp / "reconstructed"
        make_corpus(src)
        logical = sum(p.stat().st_size for p in src.rglob("*") if p.is_file())

        t0 = time.perf_counter()
        stats = pack_directory(src, store, level=3)
        pack_seconds = time.perf_counter() - t0

        t1 = time.perf_counter()
        verify = verify_store(store)
        verify_seconds = time.perf_counter() - t1

        t2 = time.perf_counter()
        unpack_store(store, out)
        unpack_seconds = time.perf_counter() - t2
        src_tree = tree_sha256(src)
        out_tree = tree_sha256(out)
        if src_tree != out_tree:
            raise RuntimeError("tree hash mismatch")

        manifest_size = (store / "manifest.json").stat().st_size
        object_files = list((store / "objects").rglob("*.zst"))
        physical = sum(p.stat().st_size for p in object_files) + manifest_size
        result = {
            "experiment": "03-blake3-cas",
            "dataset": "generated duplicate + versioned game-like files",
            "logical_bytes": logical,
            "physical_bytes": physical,
            "physical_ratio": physical / logical,
            "space_saved_ratio": 1 - physical / logical,
            "object_count": len(object_files),
            "created_chunks": stats["created_chunks"],
            "reused_chunk_references": stats["reused_chunks"],
            "manifest_bytes": manifest_size,
            "pack_seconds": pack_seconds,
            "verify_seconds": verify_seconds,
            "unpack_seconds": unpack_seconds,
            "checked_files": verify["checked_files"],
            "checked_bytes": verify["checked_bytes"],
            "source_tree_sha256": src_tree,
            "reconstructed_tree_sha256": out_tree,
            "byte_identical": src_tree == out_tree,
            "hash_algorithm": "BLAKE3-256 (dependency-free reference implementation validated against official empty/abc vectors)",
            "codec": "Zstd level 3 via CLI (experiment only)",
        }
        RESULTS.mkdir(parents=True, exist_ok=True)
        (RESULTS / "latest.json").write_text(json.dumps(result, indent=2) + "\n")
        md = f"""# Experiment 03 results

> Generated corpus only; not a commercial-game compression claim.

- Logical bytes: **{logical:,}**
- Physical CAS bytes (objects + manifest): **{physical:,}**
- Physical/logical ratio: **{result['physical_ratio']*100:.2f}%**
- Synthetic space saved: **{result['space_saved_ratio']*100:.2f}%**
- Unique object count: **{len(object_files)}**
- Reused chunk references: **{stats['reused_chunks']}**
- Pack: **{pack_seconds:.3f}s**
- Verify: **{verify_seconds:.3f}s**
- Unpack: **{unpack_seconds:.3f}s**
- Byte-identical reconstruction: **{result['byte_identical']}**
- Source tree SHA-256: `{src_tree}`
- Reconstructed tree SHA-256: `{out_tree}`

## What this proves

The prototype can turn a directory into a BLAKE3-addressed, Zstd-compressed object store, reuse identical chunks, reconstruct every file, verify every chunk hash, and reproduce the same directory-content tree hash without full-store pre-extraction.

## What this does not prove

The corpus intentionally contains duplicate and compressible data. The reported space saving is **not** an estimate for an AAA game. The current lab implementation is Python and invokes the Zstd CLI per object, so its pack/verify times are not production performance targets.
"""
        (RESULTS / "latest.md").write_text(md)
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
