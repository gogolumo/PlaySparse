#!/usr/bin/env python3
"""Verify a deterministic updater through real mounted I/O, then unmount/remount.

Generated data only. Source/base integrity and the full merged tree are compared.
Never calls an unpack operation. Keeps failed evidence and attempts cleanup.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time

spec = importlib.util.spec_from_file_location("mounted_smoke", Path(__file__).with_name("mounted-smoke.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


def tree(root):
    result = {}
    for p in sorted(root.rglob("*")):
        name = p.relative_to(root).as_posix()
        if p.is_dir():
            result[name] = {"kind": "directory"}
        elif p.is_file():
            h = hashlib.sha256()
            with p.open("rb") as f:
                for block in iter(lambda: f.read(1 << 20), b""):
                    h.update(block)
            result[name] = {"kind": "file", "bytes": p.stat().st_size, "sha256": h.hexdigest()}
        else:
            raise RuntimeError(f"unexpected special entry: {name}")
    return result


def update(root):
    # Explicit ordinary file API operations; every modified stream is flushed/fsynced.
    with (root / "data/archive-a.bin").open("r+b") as f:
        f.seek(262140)
        f.write(b"PATCH-CROSS-CHUNK-BOUNDARY")
        f.flush()
        os.fsync(f.fileno())
    with (root / "data/archive-b.bin").open("ab") as f:
        f.write(b"APPENDED-V2")
        f.flush()
        os.fsync(f.fileno())
    with (root / "data/truncate.bin").open("r+b") as f:
        f.truncate(123)
        f.truncate(8192)  # extended area must be zero-filled
        f.flush()
        os.fsync(f.fileno())
    (root / "patch").mkdir()
    with (root / "patch/new.tmp").open("xb") as f:
        f.write(b"REPLACED-ARCHIVE-V2")
        f.flush()
        os.fsync(f.fileno())
    os.replace(root / "patch/new.tmp", root / "data/replace.bin")
    (root / "data/archive-a.bin").rename(root / "data/archive-a-v2.bin")
    (root / "readme.txt").unlink()  # base tombstone
    with (root / "config.json").open("wb") as f:
        f.write(b'{"version":2,"configured":true}\n')
        f.flush()
        os.fsync(f.fileno())
    with (root / "patch/created.bin").open("xb") as f:
        f.write(b"CREATED-V2")
        f.flush()
        os.fsync(f.fileno())
    (root / "patch/created.bin").rename(root / "patch/renamed.bin")
    (root / "patch/removed.bin").write_bytes(b"temporary")
    (root / "patch/removed.bin").unlink()
    (root / "patch/empty-dir").mkdir()
    (root / "patch/empty-dir").rmdir()
    # Exercise rename of an entire base directory, including its descendants.
    (root / "old-dir").rename(root / "renamed-dir")
    with (root / "renamed-dir/nested/file.bin").open("r+b") as f:
        f.seek(2)
        f.write(b"NEW")
        f.flush()
        os.fsync(f.fileno())
    if sys.platform != "win32":
        # POSIX open handles must survive unlink and replacement.
        with (root / "patch/renamed.bin").open("r+b") as f:
            (root / "patch/renamed.bin").unlink()
            f.seek(0)
            assert f.read() == b"CREATED-V2"
            f.seek(0)
            f.write(b"ORPHAN")
            f.flush()
            os.fsync(f.fileno())
        (root / "patch/renamed.bin").write_bytes(b"FINAL-V2")


class Mount:
    def __init__(self, cli, store, path, overlay, trace, evidence, label, commands):
        self.cli, self.path, self.evidence, self.label = cli, path, evidence, label
        self.ready_file = Path("fixture.json")
        self.command = [str(cli), "mount", str(store), str(path), "--overlay", str(overlay), "--trace", str(trace), "--cache", "8M"]
        commands.append(self.command)

    def __enter__(self):
        self.stdout = (self.evidence / f"{self.label}.stdout.log").open("w")
        self.stderr = (self.evidence / f"{self.label}.stderr.log").open("w")
        self.process = subprocess.Popen(self.command, stdout=self.stdout, stderr=self.stderr)
        deadline = time.monotonic() + 30
        try:
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    raise RuntimeError(f"mount exited {self.process.returncode}")
                record = smoke.windows_mount_record(self.path) if sys.platform == "win32" else smoke.linux_mount_record(self.path)
                real = record.get("real_mount", False) if sys.platform == "win32" else os.path.ismount(self.path)
                if real and (self.path / self.ready_file).is_file():
                    if sys.platform == "linux" and not record["filesystem_type"].startswith("fuse"):
                        raise RuntimeError("not a genuine FUSE mount")
                    self.record = record or {"os_path_ismount": True}
                    return self
                time.sleep(0.03)
            raise RuntimeError("mount readiness timeout")
        except Exception:
            self.__exit__(*sys.exc_info())
            raise

    def __exit__(self, kind, value, tb):
        error = None
        try:
            if self.process.poll() is None:
                subprocess.run([str(self.cli), "unmount", str(self.path)], capture_output=True, check=True, timeout=65)
            self.process.wait(timeout=20)
            if self.process.returncode:
                raise RuntimeError(f"mount provider exited {self.process.returncode}")
        except Exception as exc:
            error = exc
        finally:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait()
            self.stdout.close()
            self.stderr.close()
            self.events = []
            for line in (self.evidence / f"{self.label}.stderr.log").read_text().splitlines():
                try:
                    v = json.loads(line)
                    if v.get("event") in ("fuse_unmounted", "winfsp_unmounted", "trace_closed"):
                        self.events.append(v)
                except json.JSONDecodeError:
                    pass
        if error:
            raise error


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--work", required=True, type=Path)
    p.add_argument("--playsparse", type=Path, default=Path("target/release/playsparse"))
    p.add_argument("--io-probe", type=Path, default=Path("target/release/io-probe"))
    args = p.parse_args()
    cli, probe, work = args.playsparse.resolve(strict=True), args.io_probe.resolve(strict=True), args.work.resolve()
    work.mkdir()  # exclusive run, never overwrite evidence
    evidence = work / "evidence"
    evidence.mkdir()
    source, store, overlay, mountpoint = (work / n for n in ("source", "base", "overlay", "mounted"))
    if sys.platform != "win32":
        mountpoint.mkdir()
    repo = Path(__file__).resolve().parent.parent
    git = ["git", "-c", f"safe.directory={repo}", "-C", str(repo)]
    report = {"version": 1, "timestamp": datetime.now(timezone.utc).isoformat(), "commit": subprocess.check_output(git + ["rev-parse", "HEAD"], text=True).strip(), "working_tree_status": subprocess.check_output(git + ["status", "--porcelain"], text=True), "os": platform.platform(), "architecture": platform.machine(), "binary_sha256": hashlib.sha256(cli.read_bytes()).hexdigest(), "status": "FAIL", "commands": [], "dataset": "generated synthetic updater; no real launcher/game compatibility claim", "windows_physical_validation": "BLOCKED_ON_PHYSICAL_WINDOWS"}
    run = lambda cmd, name: smoke.run_command(cmd, evidence, name, report["commands"])[0]
    started = time.perf_counter()
    before_source = before_base = None
    try:
        run([sys.executable, str(Path(__file__).with_name("generate-testgame.py")), str(source), "--io-probe", str(probe), "--world-bytes", str(16 << 20)], "generate")
        (source / "data").mkdir()
        (source / "data/archive-a.bin").write_bytes(bytes(range(256)) * 16384)
        (source / "data/archive-b.bin").write_bytes(b"ARCHIVE-B" * 131072)
        (source / "data/replace.bin").write_bytes(b"REPLACE-ME")
        (source / "data/truncate.bin").write_bytes(b"T" * 10000)
        (source / "old-dir/nested").mkdir(parents=True)
        (source / "old-dir/nested/file.bin").write_bytes(b"OLD-CONTENT")
        before_source = smoke.fingerprint(source)
        run([str(cli), "pack", str(source), str(store), "--chunker", "fixed", "--chunk-size", "256K"], "pack")
        run([str(cli), "verify", str(store)], "verify-before")
        before_base = smoke.fingerprint(store)
        expected = work / "expected"
        shutil.copytree(source, expected)
        update(expected)
        expected_tree = tree(expected)
        report["expected_tree"] = expected_tree
        first = Mount(cli, store, mountpoint, overlay, evidence / "update.trace.jsonl", evidence, "update-mount", report["commands"])
        with first:
            report["mount_record"] = first.record
            run([str(mountpoint / ("testgame.exe" if sys.platform == "win32" else "testgame")), "--self-test"], "mounted-executable")
            run([str(probe), str(source), str(mountpoint), "--iterations", "8", "--sequential-limit", str(8 << 20), "--output", str(evidence / "io-probe.json")], "io-probe")
            update(mountpoint)
            if tree(mountpoint) != expected_tree:
                raise RuntimeError("full mounted tree differs after update")
        report["update_metrics"] = first.events
        second = Mount(cli, store, mountpoint, overlay, evidence / "remount.trace.jsonl", evidence, "remount", report["commands"])
        with second:
            report["remount_record"] = second.record
            actual = tree(mountpoint)
            if actual != expected_tree:
                raise RuntimeError("full mounted tree differs after remount")
            report["remount_tree_verified"] = True
        report["remount_metrics"] = second.events
        report["overlay"] = run([str(cli), "overlay", "status", str(overlay)], "overlay")
        report["trace"] = run([str(cli), "trace", "summarize", str(evidence / "update.trace.jsonl")], "trace-summary")
        if report["trace"]["read_operations"] == 0 or report["trace"]["write_operations"] == 0:
            raise RuntimeError("mounted read/write trace events missing")
        run([str(cli), "verify", str(store)], "verify-after")
        report["clean_unmount"] = not mountpoint.exists() or (not os.path.ismount(mountpoint) and not any(mountpoint.iterdir()))
        if not report["clean_unmount"]:
            raise RuntimeError("mount did not cleanly detach")
        report["status"] = "PASS"
    except Exception as exc:
        report["error"] = str(exc)
    finally:
        for name, root, before in (("source", source, before_source), ("base", store, before_base)):
            if before:
                after = smoke.fingerprint(root)
                report[f"{name}_unchanged"] = before["tree_fingerprint_sha256"] == after["tree_fingerprint_sha256"]
                (evidence / f"{name}-before.json").write_text(json.dumps(before, indent=2))
                (evidence / f"{name}-after.json").write_text(json.dumps(after, indent=2))
                if not report[f"{name}_unchanged"]:
                    report["status"] = "FAIL"
        report["wall_seconds"] = time.perf_counter() - started
        if sys.platform != "win32":
            import resource
            r = resource.getrusage(resource.RUSAGE_CHILDREN)
            report["resources"] = {"child_cpu_seconds": r.ru_utime + r.ru_stime, "peak_single_child_rss_bytes": r.ru_maxrss * (1 if sys.platform == "darwin" else 1024)}
        (evidence / "result.json").write_text(json.dumps(report, indent=2))
        print(json.dumps({"status": report["status"], "evidence": str(evidence), "error": report.get("error")}, indent=2))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
