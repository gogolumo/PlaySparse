#!/usr/bin/env python3
"""Run one owned native application through a real writable mount, retaining failures."""
import argparse
from datetime import datetime, timezone
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import sys
import time

from validation_common import (CommandRunner, PrerequisiteMissing, atomic_json,
                               detach_owned_mount, digest, inspect_binary_provenance,
                               mount_present, repository_identity, safe_new_work, signals)

spec = importlib.util.spec_from_file_location("mounted_update", Path(__file__).with_name("mounted-update.py"))
updater = importlib.util.module_from_spec(spec)
spec.loader.exec_module(updater)


def validate_installation(source, executable):
    if not source.is_dir():
        raise PrerequisiteMissing("Source must be an application installation directory")
    if executable.is_absolute() or not executable.parts or any(part in ("..", ".") for part in executable.parts):
        raise PrerequisiteMissing("Executable must be a normalized relative path inside the installation")
    resolved = (source / executable).resolve(strict=True)
    if not resolved.is_relative_to(source) or not resolved.is_file():
        raise PrerequisiteMissing("Executable must remain inside the installation")
    for path in source.rglob("*"):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise PrerequisiteMissing(f"Installation contains unsupported symlink/special entry: {path.relative_to(source)}")


def fingerprint_tree(root):
    directories = []
    for path in [root, *sorted(root.rglob("*"))]:
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise RuntimeError(f"Unsupported entry while fingerprinting: {path.relative_to(root)}")
        if path.is_dir():
            metadata = path.stat()
            directories.append({"path": path.relative_to(root).as_posix(),
                                "mode": metadata.st_mode, "mtime_ns": metadata.st_mtime_ns})
    value = updater.smoke.fingerprint(root)
    files = [{key: item for key, item in row.items() if key != "allocated_bytes"} for row in value["files"]]
    value["directories"] = directories
    value["tree_fingerprint_sha256"] = hashlib.sha256(json.dumps(
        {"directories": directories, "files": files}, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    value["definition"] += "; includes directory names, permissions and modification timestamps"
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--playsparse", type=Path, required=True)
    parser.add_argument("--binary-manifest", type=Path)
    parser.add_argument("--allow-dirty", action="store_true")
    parser.add_argument("--allow-unverified-binaries", action="store_true")
    parser.add_argument("--timeout", type=int, default=1800, help="seconds per command (1..86400), including the application")
    parser.add_argument("--min-free-bytes", type=int, default=512 << 20)
    parser.add_argument("--application-kind", choices=["application", "game"], default="application")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not 1 <= args.timeout <= 86400 or not 0 <= args.min_free_bytes <= 1 << 50:
        parser.error("Timeout must be 1..86400 seconds and minimum free bytes 0..1 PiB")
    repo = Path(__file__).resolve().parent.parent
    try:
        source = args.source.expanduser().resolve(strict=True)
        cli = args.playsparse.expanduser().resolve(strict=True)
        validate_installation(source, args.executable)
        if source.is_relative_to(repo) or repo.is_relative_to(source):
            raise PrerequisiteMissing("Application installation must be outside the repository")
        work = safe_new_work(args.work, repo, source)
    except (OSError, PrerequisiteMissing) as error:
        print(json.dumps({"status": "BLOCKED", "evidence": None, "error": str(error)}))
        return 2
    evidence = work / "evidence"
    evidence.mkdir()
    store, overlay, mountpoint = (work / name for name in ("base", "overlay", "mounted"))
    if sys.platform != "win32":
        mountpoint.mkdir()
    report = {"version": 2, "timestamp": datetime.now(timezone.utc).isoformat(),
              "os": platform.platform(), "architecture": platform.machine(), "status": "FAIL",
              "evidence_type": "one user-supplied native application execution",
              "scope": "filesystem compatibility for this executable and arguments only; no launcher/DRM/anti-cheat claim",
              "real_game_validation": "NOT RUN", "game_evidence_gate": "GAME EVIDENCE REQUIRED",
              "application_kind_user_classification": args.application_kind, "commands": [],
              "mount_commands": [],
              "source": str(source), "store": str(store), "overlay": str(overlay),
              "mountpoint": str(mountpoint), "child_exit_code": None}
    runner = CommandRunner(repo, evidence, report, args.timeout)
    before_source = before_base = None
    mount = None
    started = time.perf_counter()
    atomic_json(evidence / "result.json", report)
    try:
        manifest = args.binary_manifest or cli.parent / "playsparse-build-provenance.json"
        report["binary_provenance"] = inspect_binary_provenance(
            repo, {"playsparse": cli}, manifest, args.allow_dirty, args.allow_unverified_binaries)
        report["commit"] = report["binary_provenance"]["repository"]["git_sha"]
        report["binary_sha256"] = report["binary_provenance"]["binaries"]["playsparse"]["sha256"]
        available = shutil.disk_usage(work).free
        logical = sum(path.stat().st_size for path in source.rglob("*") if path.is_file())
        # Encoded base plus full copy-up. Changing the scratch minimum never
        # bypasses the two-copy budget for a supplied application.
        required = max(args.min_free_bytes, logical * 2)
        report["disk_preflight"] = {"available_bytes": available, "required_bytes": required,
                                    "source_logical_bytes": logical,
                                    "basis": "two full logical copies plus configured scratch minimum; application writes may require more"}
        if available < required:
            raise PrerequisiteMissing("Insufficient disk for the base and possible whole-file copy-up; choose a larger work filesystem")
        before_source = fingerprint_tree(source)
        atomic_json(evidence / "source-before.json", before_source)
        report["source_before_sha256"] = before_source["tree_fingerprint_sha256"]
        # The generated fixture is identifiable, even if a caller labels it game.
        synthetic = (source / "fixture.json").is_file() and (source / "world.dat").is_file()
        report["generated_fixture_detected"] = synthetic
        runner.run([str(cli), "pack", str(source), str(store)], "pack")
        before_base = fingerprint_tree(store)
        atomic_json(evidence / "base-before.json", before_base)
        runner.run([str(cli), "verify", str(store)], "verify")
        mount = updater.Mount(cli, store, mountpoint, overlay, evidence / "trace.jsonl", evidence, "mount", report["mount_commands"])
        mount.ready_file = args.executable
        with mount:
            report["mount_record"] = mount.record
            report["mount_backend"] = "WinFsp" if sys.platform == "win32" else "macFUSE" if sys.platform == "darwin" else "FUSE"
            arguments = args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments
            command = [str(mountpoint / args.executable), *arguments]
            try:
                entry = runner.run(command, "application", cwd=mountpoint)
            finally:
                entry = next((entry for entry in reversed(report["commands"]) if isinstance(entry, dict) and entry.get("label") == "application"), {})
                report["child_exit_code"] = entry.get("exit_code")
        report["status"] = "PASS"
        if args.application_kind == "game" and not synthetic:
            report["real_game_validation"] = "PASS for one user-classified game executable and arguments; launcher compatibility NOT RUN"
            report["game_evidence_gate"] = "ONE USER-CLASSIFIED GAME EXECUTION; no general compatibility claim"
    except PrerequisiteMissing as error:
        report.update(status="BLOCKED", error=str(error))
    except Exception as error:
        report.update(status="FAIL", error=str(error), failure_type=type(error).__name__)
    finally:
        if mount is not None:
            report["unmount_metrics"] = getattr(mount, "events", [])
        try:
            if mount_present(mountpoint):
                detach_owned_mount(runner, cli, mountpoint, "cleanup-unmount")
            report["clean_unmount"] = not mount_present(mountpoint)
        except Exception as error:
            report.update(status="FAIL", cleanup_error=str(error), clean_unmount=False)
        if not report["clean_unmount"]:
            report.update(status="FAIL", cleanup_error="Mount remains attached; inspect preserved mountpoint")
        for name, root, before in (("source", source, before_source), ("base", store, before_base)):
            if before is not None:
                try:
                    after = fingerprint_tree(root)
                    atomic_json(evidence / f"{name}-after.json", after)
                    unchanged = before["tree_fingerprint_sha256"] == after["tree_fingerprint_sha256"]
                    report[f"{name}_unchanged"] = unchanged
                    report[f"{name}_after_sha256"] = after["tree_fingerprint_sha256"]
                    if not unchanged:
                        report.update(status="FAIL", integrity_error=f"{name} changed during application validation")
                except Exception as error:
                    report.update(status="FAIL", integrity_error=f"Cannot verify final {name} identity: {error}")
        if "binary_provenance" in report:
            try:
                after = repository_identity(repo)
                report["repository_after"] = after
                report["repository_unchanged_during_run"] = after == report["binary_provenance"]["repository"]
                report["binary_unchanged_during_run"] = digest(cli) == report["binary_sha256"]
                if not report["repository_unchanged_during_run"] or not report["binary_unchanged_during_run"]:
                    report.update(status="FAIL", provenance_error="Repository or runtime binary changed during application validation")
            except Exception as error:
                report.update(status="FAIL", provenance_error=str(error))
        if report["status"] != "PASS" and report["real_game_validation"].startswith("PASS"):
            report["real_game_validation"] = "FAIL"
        report["wall_seconds"] = time.perf_counter() - started
        atomic_json(evidence / "result.json", report)
        print(json.dumps({"status": report["status"], "evidence": str(evidence), "error": report.get("error")}, indent=2))
    return 0 if report["status"] == "PASS" else 2 if report["status"] == "BLOCKED" else 1


if __name__ == "__main__":
    with signals():
        sys.exit(main())
