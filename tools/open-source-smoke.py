#!/usr/bin/env python3
"""Run the system's open-source Zstd executable FROM a genuine PlaySparse mount.

Requires real FUSE/WinFsp. Creates a new generated fixture, never cleans a source
installation, and refuses reuse of an existing work directory.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import time


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def snapshot(root):
    return {p.relative_to(root).as_posix(): {'size': p.stat().st_size, 'sha256': digest(p)}
            for p in sorted(root.rglob('*')) if p.is_file()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--playsparse', required=True, type=Path)
    parser.add_argument('--work', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    exe = shutil.which('zstd')
    if not exe:
        raise SystemExit('zstd installation required for licensed open-source fixture')
    binary = args.playsparse.resolve()
    args.work.mkdir(parents=True, exist_ok=False)
    source = args.work / 'ZstdApp'
    source.mkdir()
    shutil.copy2(exe, source / ('zstd.exe' if os.name == 'nt' else 'zstd'))
    payload = (b'Open-source application reading CAS through a real filesystem.\n' * 16384)
    payload += bytes(range(256)) * 4096
    payload_hash = hashlib.sha256(payload).hexdigest()
    encoded = subprocess.run([exe, '-3', '-q', '-c'], input=payload, capture_output=True, check=True).stdout
    (source / 'fixture.zst').write_bytes(encoded)
    before = snapshot(source)
    store = args.work / 'ZstdApp.playsparse'
    packed = json.loads(subprocess.check_output([str(binary), 'pack', str(source), str(store)]))
    store_before = snapshot(store)
    mount = args.work / 'mounted'
    mount.mkdir()
    log = args.work / 'mount.log'
    start = time.perf_counter()
    with log.open('wb') as stream:
        provider = subprocess.Popen([str(binary), 'mount', str(store), str(mount), '--cache', '64M'], stdout=stream, stderr=stream)
        try:
            deadline = time.monotonic() + 20
            while not (mount / 'fixture.zst').exists():
                if provider.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError('real mount failed: ' + log.read_text(errors='replace'))
                time.sleep(0.05)
            mounted_exe = mount / Path(exe).name
            version = subprocess.check_output([str(mounted_exe), '--version'], text=True).strip()
            run_start = time.perf_counter()
            actual = subprocess.check_output([str(mounted_exe), '-d', '-q', '-c', str(mount / 'fixture.zst')])
            run_seconds = time.perf_counter() - run_start
            if hashlib.sha256(actual).hexdigest() != payload_hash or actual != payload:
                raise RuntimeError('open-source mounted application output mismatch')
            mounts = Path('/proc/mounts').read_text() if Path('/proc/mounts').exists() else None
            mount_entry = [line for line in mounts.splitlines() if str(mount) in line] if mounts else None
            after = snapshot(source)
            store_after = snapshot(store)
            if before != after or store_before != store_after:
                raise RuntimeError('source or CAS store changed during mounted execution')
        finally:
            subprocess.run([str(binary), 'unmount', str(mount)], check=True)
            try:
                provider.wait(timeout=15)
            except subprocess.TimeoutExpired:
                provider.terminate()
                provider.wait(timeout=5)
    result = {'application': 'Zstandard command line, installed OS build', 'version': version,
              'os': platform.platform(), 'architecture': platform.machine(), 'pack': packed,
              'mounted_executable': str(mounted_exe), 'mount_entry': mount_entry,
              'decoded_output_bytes': len(actual), 'output_sha256': payload_hash,
              'output_bytes_equal': True, 'source_unchanged': before == after,
              'store_unchanged': store_before == store_after, 'source_hashes': before,
              'execution_seconds': run_seconds, 'total_seconds': time.perf_counter() - start,
              'provider_log': log.read_text(errors='replace'),
              'limitations': ['Linux dynamic libraries come from the installed OS, outside mount',
                              'CLI compatibility evidence; no real game/DRM compatibility claim']}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'bytes_equal': True, 'executable_from_mount': True, 'version': version,
                      'output_sha256': payload_hash, 'output': str(args.output)}, indent=2))


if __name__ == '__main__':
    main()
