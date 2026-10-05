#!/usr/bin/env python3
"""Windows mounted validation backend for windows-hardware-validation.ps1.

Exit 0 means requested runtime stages passed. Physical/game/WOF gates remain
separate fields; exit 2 means a prerequisite blocked execution. No installers,
system compression changes or original-application execution are performed.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys

import validation_common as common
from windows_support import WindowsAPI, WindowsPrerequisite, finalizing, fingerprint, physical_gate


def require_space(path, needed):
    available = shutil.disk_usage(path).free
    if available < needed:
        raise common.PrerequisiteMissing(f"Insufficient free space: {available} bytes available, {needed} required for generated fixtures/staging; use another new evidence root")
    return {"available_bytes": available, "required_bytes": needed,
            "definition": "conservative preflight budget; later ENOSPC is still a failure"}


def stage_result(path):
    candidates = [Path(path) / "evidence" / "result.json", Path(path) / "result.json"]
    result = next((candidate for candidate in candidates if candidate.is_file()), None)
    if result is None:
        raise RuntimeError(f"stage did not publish result.json: {path}")
    with result.open("rb") as stream:
        payload = stream.read((4 << 20) + 1)
    if len(payload) > 4 << 20:
        raise RuntimeError("stage result exceeds the bounded 4 MiB limit")
    data = json.loads(payload.decode("utf-8-sig"))
    if data.get("status") != "PASS":
        raise RuntimeError(f"stage result is {data.get('status', 'missing')}: {result}")
    return {"status": "PASS", "result": str(result), "scope": data.get("scope"),
            "real_game_validation": data.get("real_game_validation", "NOT RUN"),
            "generated_fixture_detected": data.get("generated_fixture_detected")}


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--environment-stdin", action="store_true")
    parser.add_argument("--physical-machine", action="store_true")
    parser.add_argument("--game-path", type=Path)
    parser.add_argument("--executable", type=Path)
    parser.add_argument("--wof-comparison", action="store_true")
    parser.add_argument("--wof-source", type=Path)
    parser.add_argument("--wof-algorithm", choices=("XPRESS4K", "XPRESS8K", "XPRESS16K", "LZX"), default="XPRESS4K")
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--allow-dirty", action="store_true")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    return parser.parse_args(argv)


def application_command(args, cli, stage, manifest):
    arguments = args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments
    command = [sys.executable, str(Path(__file__).with_name("owned-application.py")),
               "--work", str(stage), "--playsparse", str(cli), "--source", str(args.game_path),
               "--executable", str(args.executable), "--binary-manifest", str(manifest),
               "--timeout", str(args.timeout), "--application-kind", "game"]
    if args.allow_dirty:
        command.append("--allow-dirty")
    return [*command, "--", *arguments]


def cleanup_known_mounts(cli, work, runner, report):
    """Only known harness-owned mount paths; never unmount an arbitrary source."""
    cleanup = []
    for label in ("readonly", "writable", "adaptive", "tiers", "application", "wof"):
        mount = work / label / "mounted"
        if not mount.exists():
            continue
        try:
            api = WindowsAPI()
            record = api.mount_record(mount)
            if record["real_mount"]:
                runner.run([str(cli), "unmount", str(mount)], f"cleanup-{label}", timeout=60)
                if mount.exists() and api.mount_record(mount)["real_mount"]:
                    raise RuntimeError("owned mount remains attached after unmount")
                cleanup.append({"path": str(mount), "status": "PASS"})
        except Exception as error:
            cleanup.append({"path": str(mount), "status": "FAIL", "error": str(error)})
            report["status"] = "FAIL"
    report["cleanup"] = cleanup


def run_validation(argv=None):
    args = parse_args(argv)
    repo = Path(__file__).resolve().parent.parent
    work = None
    report = {"schema": 1, "status": "FAIL", "timestamp": datetime.now(timezone.utc).isoformat(),
              "commands": [], "stages": {name: {"status": "NOT RUN"} for name in
              ("build", "doctor", "readonly", "writable", "adaptive", "tiers", "application", "wof")},
              "game_validation": "NOT RUN", "launcher_validation": "NOT RUN",
              "wof_physical_validation": "NOT RUN", "binary_repository_match": "NOT RUN"}
    cli, runner, before_source = None, None, None
    try:
        # Parsing environment is bounded and does not trust it as physical proof.
        raw = sys.stdin.read((1 << 20) + 1) if args.environment_stdin else "{}"
        if len(raw.encode()) > 1 << 20:
            raise common.PrerequisiteMissing("host environment exceeds 1 MiB")
        environment = json.loads(raw)
        environment["python"] = platform.python_version()
        environment["python_is_windows"] = sys.platform == "win32"
        work = common.safe_new_work(args.work, repo=repo, sources=[args.game_path, args.wof_source])
        for source in (args.game_path, args.wof_source):
            if source is not None:
                source = source.resolve(strict=True)
                if work.is_relative_to(source) or source.is_relative_to(work):
                    raise common.PrerequisiteMissing("Evidence root must be separate from every original source tree")
        common.atomic_json(work / "environment.json", environment)
        report["windows_physical_validation"] = physical_gate(environment, args.physical_machine)
        report["evidence_type"] = "native Windows runtime; physical classification requires user attestation and recorded client hardware"
        report["repository"] = common.repository_identity(repo)
        report["arguments"] = [sys.executable, *sys.argv]
        if bool(args.game_path) != bool(args.executable) or (args.arguments and not args.game_path):
            raise common.PrerequisiteMissing("GamePath/Executable must be supplied together; GameArguments require them")
        if args.wof_source and not args.wof_comparison:
            raise common.PrerequisiteMissing("WofSource requires WofComparison")
        if not 60 <= args.timeout <= 86400:
            raise common.PrerequisiteMissing("timeout must be 60..86400 seconds")
        if sys.platform != "win32" or not environment.get("is_windows"):
            raise common.PrerequisiteMissing("Native Windows is required; this host cannot provide Windows mounted or WOF evidence")
        if environment.get("preflight_error"):
            raise common.PrerequisiteMissing(f"Windows host discovery failed: {environment['preflight_error']}")
        if args.physical_machine and report["windows_physical_validation"]["status"] == "BLOCKED":
            raise common.PrerequisiteMissing("Physical Windows needs explicit attestation, a client Windows OS and no detected VM hints; hosted/unknown hardware remains BLOCKED")
        winfsp = environment.get("winfsp", [])
        if not winfsp or not any(item.get("sdk_headers_present") and re.match(r"^2\.1(?:\.|$)", item.get("version", "")) for item in winfsp):
            raise common.PrerequisiteMissing("Install WinFsp 2.1 including Developer SDK; this harness never installs or approves drivers")
        if report["repository"]["dirty"] and not args.allow_dirty:
            raise common.PrerequisiteMissing("Dirty repository; commit/stash changes or explicitly use -AllowDirty")
        api = WindowsAPI()
        report["work_volume"] = api.volume(work)
        if report["work_volume"]["drive_type"] != 3:
            raise common.PrerequisiteMissing("Evidence root must be on a local fixed drive")
        report["disk_space"] = require_space(work, 32 << 30)
        if args.game_path:
            before_source = fingerprint(args.game_path.resolve(strict=True), api)
            common.atomic_json(work / "game-source-before.json", before_source)
        runner = common.CommandRunner(repo, work, report, timeout=args.timeout)
        runner.run(["rustc", "-Vv"], "rustc-version", timeout=60)
        runner.run(["cargo", "-V"], "cargo-version", timeout=60)
        identity_before = common.repository_identity(repo)
        build_command = ["cargo", "build", "--locked", "--release", "--workspace"]
        runner.run(build_command, "build")
        report["stages"]["build"] = {"status": "PASS"}
        # Cargo metadata resolves CARGO_TARGET_DIR relative to the repository.
        runner.run(["cargo", "metadata", "--no-deps", "--format-version", "1"], "cargo-metadata")
        metadata = json.loads((work / "cargo-metadata.stdout.log").read_text())
        target = Path(metadata["target_directory"]) / "release"
        binaries = {"playsparse": target / "playsparse.exe", "io_probe": target / "io-probe.exe"}
        cli = binaries["playsparse"]
        manifest = work / "binary-build-manifest.json"
        common.write_build_manifest(manifest, repo, binaries, identity_before, build_command)
        report["provenance"] = common.inspect_binary_provenance(repo, binaries, manifest, allow_dirty=args.allow_dirty)
        report["binary_repository_match"] = report["provenance"]["status"]
        runner.run([str(cli), "doctor", "--path", str(work)], "doctor")
        doctor = json.loads((work / "doctor.stdout.log").read_text())
        if not doctor.get("mount_backend", {}).get("available"):
            raise common.PrerequisiteMissing(doctor.get("mount_backend", {}).get("detail", "WinFsp mount backend unavailable"))
        report["stages"]["doctor"] = {"status": "PASS", "result": str(work / "doctor.stdout.log")}
        for label, tool, extra in (
            ("readonly", "mounted-smoke.py", ["--world-bytes", "10737418240", "--iterations", "32", "--cache", "64M"]),
            ("writable", "mounted-update.py", []), ("adaptive", "adaptive-smoke.py", []),
            ("tiers", "tiered-smoke.py", []),
        ):
            stage = work / label
            report["stages"][label] = {"status": "RUNNING"}
            runner.run([sys.executable, str(repo / "tools" / tool), "--work", str(stage),
                        "--playsparse", str(cli), "--io-probe", str(binaries["io_probe"]), *extra], label)
            report["stages"][label] = stage_result(stage)
        if args.game_path:
            stage = work / "application"
            report["stages"]["application"] = {"status": "RUNNING"}
            runner.run(application_command(args, cli, stage, manifest), "application")
            report["stages"]["application"] = stage_result(stage)
            report["game_validation"] = report["stages"]["application"]["real_game_validation"]
        if args.wof_comparison:
            stage = work / "wof"
            source = args.wof_source or work / "readonly" / "TestGame"
            report["stages"]["wof"] = {"status": "RUNNING"}
            command = [sys.executable, str(repo / "tools" / "windows-comparison.py"),
                       "--work", str(stage), "--source", str(source), "--playsparse", str(cli),
                       "--binary-manifest", str(manifest), "--environment", str(work / "environment.json"),
                       "--algorithm", args.wof_algorithm, "--timeout", str(args.timeout)]
            if args.physical_machine:
                command.append("--physical-machine")
            if args.allow_dirty:
                command.append("--allow-dirty")
            runner.run(command, "wof")
            report["stages"]["wof"] = stage_result(stage)
            report["wof_physical_validation"] = "PASS" if report["windows_physical_validation"]["status"] != "BLOCKED" else "BLOCKED"
        report["status"] = "PASS"
        if report["windows_physical_validation"]["status"] != "BLOCKED":
            report["windows_physical_validation"]["status"] = "PASS"
    except (common.PrerequisiteMissing, WindowsPrerequisite) as error:
        report.update(status="BLOCKED", error=str(error))
    except BaseException as error:
        report.update(status="FAIL", error=str(error), failure_type=type(error).__name__)
    finally:
        with finalizing(report):
            for stage in report["stages"].values():
                if stage["status"] == "RUNNING":
                    stage["status"] = report["status"]
            if cli and runner and work:
                cleanup_known_mounts(cli, work, runner, report)
            if before_source:
                try:
                    after = fingerprint(args.game_path.resolve(strict=True), WindowsAPI())
                    common.atomic_json(work / "game-source-after.json", after)
                    report["game_source_unchanged"] = before_source["tree_sha256"] == after["tree_sha256"]
                    if not report["game_source_unchanged"]:
                        report.update(status="FAIL", error="original owned installation changed")
                except Exception as error:
                    report.update(status="FAIL", integrity_error=str(error))
            if work:
                if report["status"] != "PASS":
                    gate = report.get("windows_physical_validation", {})
                    if gate.get("status") == "PASS":
                        gate["status"] = "FAIL"
                    if str(report["game_validation"]).startswith("PASS"):
                        report["game_validation"] = "FAIL: validation or immutable-source check failed"
                    if report["wof_physical_validation"] == "PASS":
                        report["wof_physical_validation"] = "FAIL"
                common.atomic_json(work / "result.json", report)
            print(json.dumps({"status": report["status"], "evidence": str(work) if work else None, "error": report.get("error")}, indent=2))
    return {"PASS": 0, "FAIL": 1, "BLOCKED": 2}[report["status"]]


def main(argv=None):
    with common.signals():
        return run_validation(argv)


if __name__ == "__main__":
    sys.exit(main())
