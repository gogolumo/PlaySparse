#!/usr/bin/env python3
"""Optional local user-owned application test. No assets are uploaded or committed."""
import argparse
from datetime import datetime, timezone
import hashlib
import platform
import importlib.util
import json
from pathlib import Path
import subprocess
import sys

spec = importlib.util.spec_from_file_location("mounted_update", Path(__file__).with_name("mounted-update.py"))
updater = importlib.util.module_from_spec(spec)
spec.loader.exec_module(updater)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--playsparse", type=Path, required=True)
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    source, cli, work = args.source.resolve(strict=True), args.playsparse.resolve(strict=True), args.work.resolve()
    executable = args.executable
    if executable.is_absolute() or any(p in ("..", ".") for p in executable.parts):
        raise ValueError("Executable must be a normalized relative path inside the installation")
    resolved_executable = (source / executable).resolve(strict=True)
    if not resolved_executable.is_relative_to(source) or not resolved_executable.is_file():
        raise ValueError("Executable must remain inside the installation")
    if work.is_relative_to(source) or source.is_relative_to(work):
        raise ValueError("Work and original installation must be separate trees")
    work.mkdir()
    evidence = work / "evidence"
    evidence.mkdir()
    store, overlay, mountpoint = (work / n for n in ("base", "overlay", "mounted"))
    if sys.platform != "win32":
        mountpoint.mkdir()
    commands = []
    repo = Path(__file__).resolve().parent.parent
    git = ["git", "-c", f"safe.directory={repo}", "-C", str(repo)]
    report = {"timestamp": datetime.now(timezone.utc).isoformat(), "os": platform.platform(), "architecture": platform.machine(), "commit": subprocess.check_output(git + ["rev-parse", "HEAD"], text=True).strip(), "binary_sha256": hashlib.sha256(cli.read_bytes()).hexdigest(), "status": "FAIL", "scope": "one local owned application, filesystem compatibility only", "commands": commands}
    before = updater.smoke.fingerprint(source)
    try:
        updater.smoke.run_command([str(cli), "pack", str(source), str(store)], evidence, "pack", commands)
        before_base = updater.smoke.fingerprint(store)
        updater.smoke.run_command([str(cli), "verify", str(store)], evidence, "verify", commands)
        # Mount readiness cannot depend on a synthetic fixture for an owned application.
        mount = updater.Mount(cli, store, mountpoint, overlay, evidence / "trace.jsonl", evidence, "mount", commands)
        mount.ready_file = executable
        with mount:
            command = [str(mountpoint / executable), *(args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments)]
            commands.append(command)
            with (evidence / "application.stdout.log").open("w") as out, (evidence / "application.stderr.log").open("w") as err:
                result = subprocess.run(command, cwd=mountpoint, stdout=out, stderr=err)
            report["exit_code"] = result.returncode
            if result.returncode:
                raise RuntimeError(f"application exited {result.returncode}")
        report["unmount_metrics"] = mount.events
        report["base_unchanged"] = before_base["tree_fingerprint_sha256"] == updater.smoke.fingerprint(store)["tree_fingerprint_sha256"]
        if not report["base_unchanged"]:
            raise RuntimeError("base store changed")
        report["status"] = "PASS"
    except Exception as exc:
        report["error"] = str(exc)
    finally:
        report["source_unchanged"] = before["tree_fingerprint_sha256"] == updater.smoke.fingerprint(source)["tree_fingerprint_sha256"]
        if not report["source_unchanged"]:
            report["status"] = "FAIL"
        (evidence / "result.json").write_text(json.dumps(report, indent=2))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
