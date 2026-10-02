#!/usr/bin/env python3
"""Generate a reproducible sparse TestGame without adding binary assets to Git."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import shutil

GIB = 1 << 30


def mark_sparse(stream):
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes
        import msvcrt
        device_io = ctypes.windll.kernel32.DeviceIoControl
        device_io.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPVOID,
                              wintypes.DWORD, wintypes.LPVOID, wintypes.DWORD,
                              ctypes.POINTER(wintypes.DWORD), wintypes.LPVOID]
        returned = wintypes.DWORD()
        if not device_io(msvcrt.get_osfhandle(stream.fileno()), 0x900C4,
                         None, 0, None, 0, ctypes.byref(returned), None):
            raise ctypes.WinError()


def generate(destination, executable, logical_bytes=10 * GIB):
    destination = Path(destination)
    executable = Path(executable).resolve(strict=True)
    if destination.exists():
        raise ValueError(f"refusing to overwrite existing source: {destination}")
    if logical_bytes < 1 << 20:
        raise ValueError("world size must be at least 1 MiB")
    destination.mkdir(parents=True)
    (destination / "assets").mkdir()
    name = "testgame.exe" if executable.suffix.lower() == ".exe" else "testgame"
    shutil.copy2(executable, destination / name)
    (destination / name).chmod(0o755)
    (destination / "readme.txt").write_text("PlaySparse generated TestGame v1\n", encoding="utf-8")
    (destination / "config.json").write_text(json.dumps({"title": "TestGame", "seed": 5042, "world": "world.dat", "audio": "audio.dat"}, sort_keys=True) + "\n")
    (destination / "assets" / "empty.bin").touch()
    (destination / "assets" / "texture.dat").write_bytes((b"TEXTURE" + bytes(range(256))) * 4096)
    rng = random.Random(5042)
    (destination / "assets" / "entropy.dat").write_bytes(rng.randbytes(2 << 20))
    (destination / "audio.dat").write_bytes((b"AUDIO" + bytes(range(256))) * 8192)
    markers = {0, 262_144 - 2048, 1 << 20, logical_bytes - 65_536}
    markers.update(p for p in [(4 * GIB) - 2048, (8 * GIB) - 2048] if 0 <= p < logical_bytes - 65_536)
    markers.update(rng.randrange(logical_bytes - 65_536) for _ in range(64))
    world = destination / "world.dat"
    with world.open("wb") as stream:
        mark_sparse(stream)
        stream.truncate(logical_bytes)
        for position in sorted(markers):
            stream.seek(position)
            seed = hashlib.sha256(f"TestGame-v1-{position}".encode()).digest()
            stream.write(seed * 2048)
    files = []
    for relative in ["config.json", "readme.txt", "audio.dat", "assets/texture.dat", "assets/entropy.dat", "assets/empty.bin", "world.dat"]:
        path = destination / relative
        size = path.stat().st_size
        offsets = {0, max(0, size - 37), size, size + 1}
        if relative == "world.dat":
            offsets.update(markers)
            offsets.update(max(0, p - 37) for p in markers)
            offsets.update(p for p in [262_143, 262_144, 4 * GIB - 1, 4 * GIB + 123, 8 * GIB - 1, 8 * GIB + 123] if p < size)
        else:
            offsets.update([size // 2, max(0, size - 4096)])
        samples = []
        with path.open("rb") as stream:
            for position in sorted(offsets):
                stream.seek(position)
                samples.append({"offset": position, "data_hex": stream.read(4096).hex()})
        files.append({"path": relative, "size": size, "samples": samples})
    fixture = {"schema": 1, "dataset": "synthetic sparse TestGame; not a real game compression estimate", "world_logical_bytes": logical_bytes, "seed": 5042, "files": files}
    (destination / "fixture.json").write_text(json.dumps(fixture, sort_keys=True, separators=(",", ":")) + "\n")
    paths = [p for p in destination.rglob("*") if p.is_file()]
    summary = {"source": str(destination.resolve()), "executable": name, "world_logical_bytes": logical_bytes, "logical_bytes": sum(p.stat().st_size for p in paths), "files": len(paths), "source_allocated_bytes": sum(getattr(p.stat(), "st_blocks", 0) * 512 for p in paths), "nonzero_world_regions": len(markers), "sample_count": sum(len(f["samples"]) for f in files)}
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--io-probe", type=Path, default=Path("target/release/io-probe"))
    parser.add_argument("--world-bytes", type=int, default=10 * GIB)
    args = parser.parse_args()
    print(json.dumps(generate(args.destination, args.io_probe, args.world_bytes), indent=2))


if __name__ == "__main__":
    main()
