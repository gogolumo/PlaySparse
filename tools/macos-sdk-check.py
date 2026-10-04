#!/usr/bin/env python3
"""Compile and test the macFUSE backend against a verified SDK without installing it.

This is compiler/library evidence, not native mount validation. The official
installer image is attached read-only, extracted locally, and detached normally.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time
import urllib.request

VERSION = "5.4.0"
SHA256 = "861814f0ac7fa8f6547ea40cdd49a36ac84bcc7d34f38a1fa74e8cf68b0401c5"
URL = f"https://github.com/macfuse/macfuse/releases/download/macfuse-{VERSION}/macfuse-{VERSION}.dmg"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("This SDK check requires native macOS")
    work = args.work.expanduser().resolve()
    work.mkdir(parents=True, exist_ok=False)
    repo = Path(__file__).resolve().parent.parent
    report = {"version": 1, "status": "FAIL", "timestamp": datetime.now(timezone.utc).isoformat(),
              "os": platform.platform(), "architecture": platform.machine(), "commands": [],
              "sdk_version": VERSION, "url": URL, "expected_sha256": SHA256,
              "source_hashes": {str(path.relative_to(repo)): hashlib.sha256(path.read_bytes()).hexdigest()
                                for path in (repo / "crates/playsparse-vfs-fuse").rglob("*.rs")},
              "driver_installed_by_this_run": False, "mount_validation": "NOT RUN",
              "git_sha": subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip(),
              "working_tree_status": subprocess.check_output(["git", "-C", str(repo), "status", "--porcelain"], text=True)}
    mountpoint = None
    started = time.perf_counter()

    def save():
        (work / "result.json").write_text(json.dumps(report, indent=2) + "\n")

    def run(command, label, env=None):
        before = time.perf_counter()
        with (work / f"{label}.stdout.log").open("w") as out, (work / f"{label}.stderr.log").open("w") as err:
            result = subprocess.run(command, cwd=repo, env=env, stdout=out, stderr=err)
        report["commands"].append({"argv": command, "label": label, "exit_code": result.returncode,
                                   "wall_seconds": time.perf_counter() - before})
        save()
        print(f"{label}: {result.returncode}", flush=True)
        if result.returncode:
            raise RuntimeError(f"{label} failed; inspect retained logs")

    try:
        image = work / f"macfuse-{VERSION}.dmg"
        digest = hashlib.sha256()
        size = 0
        with urllib.request.urlopen(URL, timeout=60) as response, image.open("xb") as out:
            while block := response.read(1 << 20):
                size += len(block)
                if size > 64 << 20:
                    raise RuntimeError("installer image exceeds the download size limit")
                digest.update(block)
                out.write(block)
        report.update(download_bytes=size, actual_sha256=digest.hexdigest())
        if digest.hexdigest() != SHA256:
            raise RuntimeError("macFUSE image checksum mismatch")
        mountpoint = work / "image"
        mountpoint.mkdir()
        run(["hdiutil", "attach", "-readonly", "-nobrowse", "-noautoopen", "-mountpoint", str(mountpoint), str(image)], "attach")
        package = mountpoint / "Install macFUSE.pkg"
        run(["pkgutil", "--check-signature", str(package)], "signature")
        signature = (work / "signature.stdout.log").read_text()
        if "Benjamin Fleischer (3T5GSNBU6W)" not in signature or "trusted by the Apple notary service" not in signature:
            raise RuntimeError("unexpected installer signing identity or notarization status")
        run(["pkgutil", "--expand-full", str(package), str(work / "sdk")], "extract")
        payload = work / "sdk/Core.pkg/Payload"
        env = dict(os.environ)
        env.update(PKG_CONFIG_PATH=str(payload / "usr/local/lib/pkgconfig"),
                   PKG_CONFIG_SYSROOT_DIR=str(payload),
                   DYLD_LIBRARY_PATH=str(payload / "usr/local/lib"),
                   DYLD_FRAMEWORK_PATH=str(payload / "Library/Filesystems/macfuse.fs/Contents/Frameworks"),
                   CARGO_TARGET_DIR=os.environ.get("CARGO_TARGET_DIR", str(work / "target")))
        cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
        run([cargo, "fmt", "--all", "--", "--check"], "fmt", env)
        run([cargo, "clippy", "--locked", "--workspace", "--all-targets", "--features", "macfuse", "--", "-D", "warnings"], "clippy", env)
        run([cargo, "test", "--locked", "--workspace", "--features", "macfuse"], "tests", env)
        run([cargo, "build", "--locked", "--release", "--workspace", "--features", "macfuse"], "release", env)
        report["status"] = "PASS"
    except Exception as error:
        report["error"] = str(error)
    finally:
        if mountpoint and os.path.ismount(mountpoint):
            try:
                run(["hdiutil", "detach", str(mountpoint)], "detach")
            except Exception as error:
                report.update(status="FAIL", cleanup_error=str(error))
        report["wall_seconds"] = time.perf_counter() - started
        save()
    print(json.dumps({"status": report["status"], "evidence": str(work), "mount_validation": "NOT RUN"}, indent=2))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
