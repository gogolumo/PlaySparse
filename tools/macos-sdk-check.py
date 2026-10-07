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
import sys
import time
import urllib.request

import validation_common as common

VERSION = "5.4.0"
SHA256 = "861814f0ac7fa8f6547ea40cdd49a36ac84bcc7d34f38a1fa74e8cf68b0401c5"
URL = f"https://github.com/macfuse/macfuse/releases/download/macfuse-{VERSION}/macfuse-{VERSION}.dmg"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--allow-dirty", action="store_true", help="record an explicitly accepted development checkout")
    parser.add_argument("--stage-timeout", type=int, default=1800)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("This SDK check requires native macOS")
    repo = Path(__file__).resolve().parent.parent
    try:
        work = common.safe_new_work(args.work, repo=repo)
    except common.PrerequisiteMissing as error:
        print(json.dumps({"status": "BLOCKED", "error": str(error)}))
        return 2
    report = {"version": 1, "status": "FAIL", "timestamp": datetime.now(timezone.utc).isoformat(),
              "os": platform.platform(), "architecture": platform.machine(), "commands": [],
              "sdk_version": VERSION, "url": URL, "expected_sha256": SHA256,
              "driver_installed_by_this_run": False, "mount_validation": "NOT RUN",
              "allow_dirty": args.allow_dirty, "image_cleanup": "NOT RUN"}
    mountpoint = None
    started = time.perf_counter()

    def save():
        common.atomic_json(work / "result.json", report)

    runner = common.CommandRunner(repo, work, report, timeout=args.stage_timeout)

    def run(command, label, env=None):
        result = runner.run(command, label, env=env, timeout=90 if label == "detach" else None)
        print(f"{label}: {result['exit_code']}", flush=True)

    try:
        save()
        if not 60 <= args.stage_timeout <= 86400:
            raise common.PrerequisiteMissing("stage-timeout must be 60..86400 seconds")
        identity = common.repository_identity(repo)
        report.update(repository=identity, git_sha=identity["git_sha"], working_tree_status=identity["working_tree_status"])
        if identity["dirty"] and not args.allow_dirty:
            raise common.PrerequisiteMissing("Dirty repository; commit/stash changes or explicitly use --allow-dirty")
        report["source_hashes"] = {str(path.relative_to(repo)): common.digest(path)
                                   for path in (repo / "crates/playsparse-vfs-fuse").rglob("*.rs")}
        image = work / f"macfuse-{VERSION}.dmg"
        digest = hashlib.sha256()
        size = 0
        with urllib.request.urlopen(URL, timeout=60) as response, image.open("xb") as out:
            while block := response.read(1 << 20):
                if time.perf_counter() - started > args.stage_timeout:
                    raise TimeoutError("installer image download exceeded stage-timeout")
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
        run(["pkgutil", "--check-signature", str(package)], "signature",
            dict(os.environ, LC_ALL="C", LANG="C"))
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
        # Run the SDK-linked workspace suite serially. Some tests intentionally
        # spawn short-lived child processes; on macOS a fork can briefly inherit
        # another test's advisory library-lock descriptor before exec closes it.
        # Serial execution keeps this compile/link validation deterministic
        # without weakening the production lock itself.
        run([cargo, "test", "--locked", "--workspace", "--features", "macfuse", "--", "--test-threads=1"], "tests", env)
        run([cargo, "build", "--locked", "--release", "--workspace", "--features", "macfuse"], "release", env)
        report["status"] = "PASS"
    except common.PrerequisiteMissing as error:
        report.update(status="BLOCKED", error=str(error))
    except BaseException as error:
        report.update(status="FAIL", error=str(error), failure_type=type(error).__name__)
    finally:
        if mountpoint:
            try:
                if common.mount_present(mountpoint):
                    run(["hdiutil", "detach", str(mountpoint)], "detach")
                if common.mount_present(mountpoint):
                    raise RuntimeError("installer image remains attached after cleanup")
                report["image_cleanup"] = "PASS"
            except BaseException as error:
                report.update(status="FAIL", cleanup_error=str(error), image_cleanup="FAIL")
        if "identity" in locals():
            try:
                after = common.repository_identity(repo)
                report["repository_after"] = after
                report["repository_unchanged_during_run"] = after == identity
                if after != identity:
                    report.update(status="FAIL", provenance_error="Repository changed during SDK validation; repeat from a stable checkout")
            except Exception as error:
                report.update(status="FAIL", provenance_error=str(error))
        report["wall_seconds"] = time.perf_counter() - started
        save()
    print(json.dumps({"status": report["status"], "evidence": str(work), "mount_validation": "NOT RUN"}, indent=2))
    return {"PASS": 0, "FAIL": 1, "BLOCKED": 2}[report["status"]]


if __name__ == "__main__":
    with common.signals():
        sys.exit(main())
