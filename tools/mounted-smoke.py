#!/usr/bin/env python3
"""Pack, genuinely mount, compare I/O and launch the mounted TestGame executable.

Never extracts data. Refuses an existing work directory. Evidence is written even
when a stage fails. Source fingerprints include all allocated data extents plus
logical size and hole placement, avoiding a 10 GiB read just to hash sparse zeros.
"""
import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import time


def fingerprint(root):
    rows = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        stat = path.stat()
        digest = hashlib.sha256()
        extents = []
        with path.open("rb") as stream:
            position = 0
            sparse_supported = hasattr(os, "SEEK_DATA") and hasattr(os, "SEEK_HOLE")
            while position < stat.st_size:
                if sparse_supported:
                    try:
                        data = os.lseek(stream.fileno(), position, os.SEEK_DATA)
                        end = min(os.lseek(stream.fileno(), data, os.SEEK_HOLE), stat.st_size)
                    except OSError as error:
                        if error.errno == errno.ENXIO:
                            break
                        if error.errno not in (errno.EINVAL, errno.ENOTSUP):
                            raise
                        sparse_supported = False
                        data, end = position, stat.st_size
                else:
                    data, end = position, stat.st_size
                extents.append([data, end])
                digest.update(data.to_bytes(8, "little"))
                digest.update(end.to_bytes(8, "little"))
                stream.seek(data)
                remaining = end - data
                while remaining:
                    block = stream.read(min(1 << 20, remaining))
                    if not block:
                        raise IOError(f"unexpected EOF fingerprinting {path}")
                    digest.update(block)
                    remaining -= len(block)
                position = end
        rows.append({"path": path.relative_to(root).as_posix(), "size": stat.st_size, "mode": stat.st_mode, "mtime_ns": stat.st_mtime_ns, "allocated_bytes": stat.st_blocks * 512 if hasattr(stat, "st_blocks") else None, "extents": extents, "extent_sha256": digest.hexdigest()})
    # st_blocks can change when the filesystem finishes allocation bookkeeping,
    # even though bytes, holes, permissions and modification time are unchanged.
    # Retain allocation measurements, but exclude them from source identity.
    identity = [{key: value for key, value in row.items() if key != "allocated_bytes"} for row in rows]
    canonical = json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()
    return {"definition": "SHA-256 of ordered path, size, mode, mtime, extent positions and contents; allocation accounting excluded; not a whole-file SHA-256", "tree_fingerprint_sha256": hashlib.sha256(canonical).hexdigest(), "files": rows}


def run_command(command, evidence, name, commands):
    commands.append([str(v) for v in command])
    start = time.perf_counter()
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    (evidence / f"{name}.stdout.log").write_text(result.stdout)
    (evidence / f"{name}.stderr.log").write_text(result.stderr)
    if result.returncode:
        raise RuntimeError(f"{name} failed ({result.returncode}): {result.stderr[-4000:]}")
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError:
        value = {"stdout": result.stdout}
    (evidence / f"{name}.json").write_text(json.dumps(value, indent=2))
    return value, time.perf_counter() - start


def linux_mount_record(mountpoint):
    if sys.platform != "linux":
        return None
    for row in Path("/proc/self/mountinfo").read_text().splitlines():
        fields = row.split()
        if fields[4].replace("\\040", " ") == str(mountpoint):
            separator = fields.index("-")
            return {"mountinfo": row, "filesystem_type": fields[separator + 1]}
    return None


def windows_mount_record(mountpoint):
    if sys.platform != "win32":
        return None
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    create = kernel.CreateFileW
    create.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                       wintypes.LPVOID, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    create.restype = wintypes.HANDLE
    close = kernel.CloseHandle
    close.argtypes = [wintypes.HANDLE]
    close.restype = wintypes.BOOL
    query = kernel.GetVolumeInformationByHandleW
    query.argtypes = [wintypes.HANDLE, wintypes.LPWSTR, wintypes.DWORD,
                     ctypes.POINTER(wintypes.DWORD), ctypes.POINTER(wintypes.DWORD),
                     ctypes.POINTER(wintypes.DWORD), wintypes.LPWSTR, wintypes.DWORD]
    query.restype = wintypes.BOOL
    # Query the directory's resolved handle: directory mounts are reparse points,
    # and a drive-root query can describe the underlying NTFS volume instead.
    handle = create(str(mountpoint), 0x80, 7, None, 3, 0x02000000, None)
    if handle == ctypes.c_void_p(-1).value:
        return {"real_mount": False, "api": "CreateFileW", "error": ctypes.get_last_error()}
    filesystem = ctypes.create_unicode_buffer(261)
    label = ctypes.create_unicode_buffer(261)
    try:
        ok = query(handle, label, len(label), None, None, None, filesystem, len(filesystem))
        if not ok:
            return {"real_mount": False, "api": "GetVolumeInformationByHandleW", "error": ctypes.get_last_error()}
        return {"real_mount": filesystem.value == "PlaySparse", "filesystem_type": filesystem.value, "volume_label": label.value, "api": "GetVolumeInformationByHandleW"}
    finally:
        close(handle)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--playsparse", type=Path, default=Path("target/release/playsparse"))
    parser.add_argument("--io-probe", type=Path, default=Path("target/release/io-probe"))
    parser.add_argument("--world-bytes", type=int, default=10 << 30)
    parser.add_argument("--iterations", type=int, default=64)
    parser.add_argument("--cache", default="64M")
    parser.add_argument("--chunker", choices=["cdc", "fixed"], default="fixed")
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--sequential-limit", type=int, default=64 << 20)
    args = parser.parse_args()
    work = args.work.resolve()
    playsparse = args.playsparse.resolve(strict=True)
    probe = args.io_probe.resolve(strict=True)
    if work.exists():
        raise ValueError(f"refusing to overwrite existing run directory: {work}")
    work.mkdir(parents=True)
    evidence = work / "evidence"
    evidence.mkdir()
    source, store, mountpoint = work / "TestGame", work / "TestGame.playsparse", work / "mounted"
    # WinFsp creates and removes a directory mountpoint itself.
    if sys.platform != "win32":
        mountpoint.mkdir()
    commands = []
    report = {"schema": 1, "os": platform.platform(), "python": platform.python_version(), "commands": commands, "source": str(source), "store": str(store), "mountpoint": str(mountpoint), "status": "FAIL", "windows_physical_test": "REQUIRED", "windows_native_runtime_test": "NOT RUN"}
    process = None
    mounted = False
    before = None
    start = time.perf_counter()
    try:
        repo = Path(__file__).resolve().parent.parent
        # The Linux CI mount process runs as root in a checkout owned by runner.
        git = ["git", "-c", f"safe.directory={repo}", "-C", str(repo)]
        report["commit"] = subprocess.check_output(git + ["rev-parse", "HEAD"], text=True).strip()
        report["working_tree_status"] = subprocess.check_output(git + ["status", "--porcelain"], text=True)
        report["binary_sha256"] = {str(binary): hashlib.sha256(binary.read_bytes()).hexdigest() for binary in [playsparse, probe]}
        generator = Path(__file__).with_name("generate-testgame.py")
        report["dataset"], _ = run_command([sys.executable, str(generator), str(source), "--io-probe", str(probe), "--world-bytes", str(args.world_bytes)], evidence, "generate", commands)
        before = fingerprint(source)
        (evidence / "source-before.json").write_text(json.dumps(before, indent=2))
        report["pack"], report["pack_wall_seconds"] = run_command([str(playsparse), "pack", str(source), str(store), "--layout", "packs", "--chunker", args.chunker, "--chunk-size", "256K"], evidence, "pack", commands)
        report["verify"], _ = run_command([str(playsparse), "verify", str(store)], evidence, "verify", commands)
        original_self, _ = run_command([str(source / report["dataset"]["executable"]), "--self-test"], evidence, "original-self-test", commands)
        command = [str(playsparse), "mount", str(store), str(mountpoint), "--cache", args.cache]
        commands.append(command)
        with (evidence / "mount.stdout.log").open("w") as stdout, (evidence / "mount.stderr.log").open("w") as stderr:
            process = subprocess.Popen(command, stdout=stdout, stderr=stderr)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(f"mount exited {process.returncode}: {(evidence / 'mount.stderr.log').read_text()[-4000:]}")
            record = windows_mount_record(mountpoint) if sys.platform == "win32" else linux_mount_record(mountpoint)
            if sys.platform == "win32":
                report["last_windows_mount_observation"] = record
            real_mount = record["real_mount"] if sys.platform == "win32" else os.path.ismount(mountpoint)
            if real_mount and (mountpoint / "fixture.json").is_file():
                if sys.platform == "linux" and (record is None or not record["filesystem_type"].startswith("fuse")):
                    raise RuntimeError("mount is not a real FUSE filesystem")
                mounted = True
                report["mount_record"] = record or {"os_path_ismount": True}
                break
            time.sleep(0.05)
        if not mounted:
            raise RuntimeError("real filesystem mount did not become ready within 30 seconds")
        candidate_self, _ = run_command([str(mountpoint / report["dataset"]["executable"]), "--self-test"], evidence, "mounted-self-test", commands)
        if candidate_self["sample_blake3"] != original_self["sample_blake3"]:
            raise RuntimeError("mounted executable and original executable sample checksums differ")
        report["mounted_executable"] = candidate_self
        probe_command = [str(probe), str(source), str(mountpoint), "--iterations", str(args.iterations), "--sequential-limit", str(args.sequential_limit), "--output", str(evidence / "io-probe.json")]
        if args.baseline:
            probe_command.extend(["--baseline", str(args.baseline.resolve())])
        report["io_probe"], report["probe_wall_seconds"] = run_command(probe_command, evidence, "io-probe", commands)
        report["status"] = "PASS"
        if sys.platform == "win32":
            report["windows_native_runtime_test"] = "PASS"
    except Exception as error:
        report["error"] = str(error)
    finally:
        if mounted or (process is not None and process.poll() is None):
            try:
                run_command([str(playsparse), "unmount", str(mountpoint)], evidence, "unmount", commands)
            except Exception as error:
                report["unmount_error"] = str(error)
                report["status"] = "FAIL"
        if process is not None:
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                if sys.platform == "win32":
                    process.terminate()
                else:
                    process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            report["mount_exit_code"] = process.returncode
        if before is not None:
            after = fingerprint(source)
            (evidence / "source-after.json").write_text(json.dumps(after, indent=2))
            report["source_untouched"] = before["tree_fingerprint_sha256"] == after["tree_fingerprint_sha256"]
            report["source_allocation_changed"] = {row["path"]: row["allocated_bytes"] for row in before["files"]} != {row["path"]: row["allocated_bytes"] for row in after["files"]}
            report["source_fingerprint_sha256"] = after["tree_fingerprint_sha256"]
            if not report["source_untouched"]:
                report["status"] = "FAIL"
        report["no_pre_extraction"] = {"basis": "OS-verified filesystem mount; empty or absent mountpoint after unmount; runner never calls unpack/extraction", "mountpoint_empty_after_unmount": not mountpoint.exists() or (not os.path.ismount(mountpoint) and not any(mountpoint.iterdir()))}
        report["wall_seconds"] = time.perf_counter() - start
        if sys.platform != "win32":
            import resource
            usage = resource.getrusage(resource.RUSAGE_CHILDREN)
            report["child_process_resources"] = {
                "cpu_seconds": usage.ru_utime + usage.ru_stime,
                "peak_single_child_rss_bytes": usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024),
                "definition": "CPU summed across completed direct children (pack, verify, mount daemon, probe and testgame); RSS is maximum individual child, not summed resident memory"}
        (evidence / "result.json").write_text(json.dumps(report, indent=2))
        print(json.dumps({"status": report["status"], "evidence": str(evidence), "error": report.get("error")}, indent=2))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
