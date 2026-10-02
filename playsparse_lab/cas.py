"""Research Content-Addressable Store for PlaySparse Experiment 03."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from .blake3_ref import hexdigest as blake3_hex
from .chunking import fastcdc_chunks

FORMAT_VERSION = 0


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _zstd_compress(data: bytes, level: int) -> bytes:
    proc = subprocess.run(
        ["zstd", f"-{level}", "-q", "-c"],
        input=data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return proc.stdout


def _zstd_decompress(data: bytes) -> bytes:
    proc = subprocess.run(
        ["zstd", "-d", "-q", "-c"],
        input=data,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return proc.stdout


def _object_path(store: Path, digest: str) -> Path:
    return store / "objects" / digest[:2] / f"{digest[2:]}.zst"


def pack_directory(source: Path, store: Path, *, level: int = 3) -> dict:
    source = source.resolve()
    store = store.resolve()
    if shutil.which("zstd") is None:
        raise RuntimeError("zstd CLI not found")
    store.mkdir(parents=True, exist_ok=True)
    (store / "objects").mkdir(exist_ok=True)

    files = []
    logical_bytes = 0
    referenced_chunk_bytes = 0
    new_object_raw_bytes = 0
    new_object_stored_bytes = 0
    reused_chunks = 0
    created_chunks = 0

    for path in sorted(p for p in source.rglob("*") if p.is_file()):
        rel = path.relative_to(source).as_posix()
        data = path.read_bytes()
        logical_bytes += len(data)
        entries = []
        for chunk in fastcdc_chunks(data):
            raw = chunk.data
            digest = blake3_hex(raw)
            obj = _object_path(store, digest)
            referenced_chunk_bytes += len(raw)
            if obj.exists():
                reused_chunks += 1
                stored_size = obj.stat().st_size
            else:
                comp = _zstd_compress(raw, level)
                obj.parent.mkdir(parents=True, exist_ok=True)
                fd, tmp_name = tempfile.mkstemp(prefix=".tmp-", dir=obj.parent)
                try:
                    with os.fdopen(fd, "wb") as f:
                        f.write(comp)
                        f.flush()
                        os.fsync(f.fileno())
                    os.replace(tmp_name, obj)
                finally:
                    if os.path.exists(tmp_name):
                        os.unlink(tmp_name)
                created_chunks += 1
                new_object_raw_bytes += len(raw)
                new_object_stored_bytes += len(comp)
                stored_size = len(comp)
            entries.append({
                "hash": f"blake3:{digest}",
                "offset": chunk.offset,
                "raw_size": len(raw),
                "stored_size": stored_size,
                "codec": "zstd",
            })
        files.append({
            "path": rel,
            "size": len(data),
            "sha256": _sha256(data),
            "mode": path.stat().st_mode & 0o777,
            "chunks": entries,
        })

    manifest = {
        "format": "playsparse-store",
        "version": FORMAT_VERSION,
        "chunker": {
            "algorithm": "fastcdc-style-gear-v0",
            "min": 64 * 1024,
            "avg": 256 * 1024,
            "max": 1024 * 1024,
        },
        "hash": "blake3-256",
        "codec": {"name": "zstd", "level": level},
        "files": files,
    }
    manifest_bytes = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
    tmp_manifest = store / ".manifest.json.tmp"
    tmp_manifest.write_bytes(manifest_bytes)
    os.replace(tmp_manifest, store / "manifest.json")

    physical_objects = sum(p.stat().st_size for p in (store / "objects").rglob("*.zst"))
    return {
        "logical_bytes": logical_bytes,
        "physical_object_bytes": physical_objects,
        "manifest_bytes": len(manifest_bytes),
        "created_chunks": created_chunks,
        "reused_chunks": reused_chunks,
        "new_object_raw_bytes": new_object_raw_bytes,
        "new_object_stored_bytes": new_object_stored_bytes,
    }


def load_manifest(store: Path) -> dict:
    return json.loads((store / "manifest.json").read_text())


def reconstruct_file(store: Path, file_entry: dict) -> bytes:
    output = bytearray()
    for entry in file_entry["chunks"]:
        digest = entry["hash"].split(":", 1)[1]
        obj = _object_path(store, digest)
        if not obj.exists():
            raise FileNotFoundError(f"missing object {digest}")
        raw = _zstd_decompress(obj.read_bytes())
        if len(raw) != entry["raw_size"]:
            raise ValueError(f"size mismatch for {digest}")
        if blake3_hex(raw) != digest:
            raise ValueError(f"BLAKE3 mismatch for {digest}")
        output.extend(raw)
    data = bytes(output)
    if len(data) != file_entry["size"]:
        raise ValueError(f"file size mismatch: {file_entry['path']}")
    if _sha256(data) != file_entry["sha256"]:
        raise ValueError(f"file SHA-256 mismatch: {file_entry['path']}")
    return data


def verify_store(store: Path) -> dict:
    manifest = load_manifest(store)
    checked_files = 0
    checked_bytes = 0
    for entry in manifest["files"]:
        data = reconstruct_file(store, entry)
        checked_files += 1
        checked_bytes += len(data)
    return {"checked_files": checked_files, "checked_bytes": checked_bytes, "ok": True}


def unpack_store(store: Path, destination: Path) -> None:
    manifest = load_manifest(store)
    destination.mkdir(parents=True, exist_ok=True)
    for entry in manifest["files"]:
        out = destination / entry["path"]
        out.parent.mkdir(parents=True, exist_ok=True)
        data = reconstruct_file(store, entry)
        out.write_bytes(data)
        try:
            os.chmod(out, entry.get("mode", 0o644))
        except PermissionError:
            pass


def tree_sha256(root: Path) -> str:
    h = hashlib.sha256()
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        rel = path.relative_to(root).as_posix().encode()
        data = path.read_bytes()
        h.update(len(rel).to_bytes(4, "little"))
        h.update(rel)
        h.update(len(data).to_bytes(8, "little"))
        h.update(data)
    return h.hexdigest()
