#!/usr/bin/env python3
"""Run genuine Linux FUSE or native macFUSE validation, retaining all evidence.

No dependency installation, privilege escalation or extraction. The work directory
must be new. A generated fixture is never reported as a real game test.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import shutil
import stat
from validation_common import (CommandRunner, PrerequisiteMissing, atomic_json, digest,
                               detach_owned_mount, inspect_binary_provenance,
                               mount_present, repository_identity,
                               safe_new_work, signals, write_build_manifest)
import subprocess
import sys
import time


write_json = atomic_json

def optional_text(path):
    try:
        with Path(path).open(encoding="utf-8") as stream:
            return stream.read(65536).strip()
    except OSError:
        return None


def observation(command):
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=10)
        return {"command": command, "exit_code": result.returncode,
                "stdout": result.stdout[:65536].strip(), "stderr": result.stderr[:65536].strip()}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"command": command, "error": str(error)}


def environment(repo, work):
    git = ["git", "-c", f"safe.directory={repo}", "-C", str(repo)]
    status = observation(git + ["status", "--porcelain"])
    result = {"timestamp": datetime.now(timezone.utc).isoformat(),
              "os": platform.platform(), "system": platform.system(),
              "kernel": platform.release(), "architecture": platform.machine(),
              "python": platform.python_version(), "cpu_count": os.cpu_count(),
              "uid": os.getuid() if hasattr(os, "getuid") else None, "git": observation(git + ["rev-parse", "HEAD"]),
              "working_tree": status,
              "source_clean": status.get("exit_code") == 0 and not status.get("stdout"),
              "available_disk_bytes": shutil.disk_usage(work).free,
              "physical_hardware_status": "UNKNOWN; virtualization hints are not a physical-machine attestation"}
    if sys.platform == "linux":
        result["distribution"] = optional_text("/etc/os-release")
        cpuinfo = optional_text("/proc/cpuinfo") or ""
        result["cpu_model"] = next((row.split(":", 1)[1].strip() for row in cpuinfo.splitlines()
                                    if row.startswith(("model name", "Hardware"))), None)
        memory = optional_text("/proc/meminfo") or ""
        result["ram_bytes"] = next((int(row.split()[1]) * 1024 for row in memory.splitlines()
                                    if row.startswith("MemTotal:")), None)
        result["virtualization_hints"] = {
            "docker_marker": Path("/.dockerenv").exists(),
            "container_marker": Path("/run/.containerenv").exists(),
            "system_vendor": optional_text("/sys/class/dmi/id/sys_vendor"),
            "product_name": optional_text("/sys/class/dmi/id/product_name"),
            "linuxkit_kernel": "linuxkit" in platform.release().lower(),
        }
        helper = shutil.which("fusermount3") or shutil.which("fusermount")
        result["fuse_helper"] = observation([helper, "--version"]) if helper else None
        result["fuse_device"] = {"path": "/dev/fuse", "exists": Path("/dev/fuse").exists()}
        try:
            metadata = Path("/dev/fuse").stat()
            if not stat.S_ISCHR(metadata.st_mode):
                raise OSError("/dev/fuse is not a character device")
            descriptor = os.open("/dev/fuse", os.O_RDWR)
            os.close(descriptor)
            result["fuse_device"]["opened_read_write"] = True
        except OSError as error:
            result["fuse_device"].update(opened_read_write=False, error=str(error))
    elif sys.platform == "darwin":
        result["hardware_model"] = observation(["/usr/sbin/sysctl", "-n", "hw.model"])
        result["ram"] = observation(["/usr/sbin/sysctl", "-n", "hw.memsize"])
        driver = Path("/Library/Filesystems/macfuse.fs")
        result["macfuse"] = {"filesystem_bundle": str(driver), "installed": driver.is_dir(),
                             "framework_directory_present": (driver / "Contents/Frameworks").is_dir(),
                             "driver_approval_and_load_status": "UNKNOWN; a successful mount is required"}
        result["unmount_helper"] = Path("/sbin/umount").is_file()
    return result


def run(command, label, repo, work, report, timeout):
    print(f"{label}: running", file=sys.stderr, flush=True)
    return CommandRunner(repo, work, report, timeout).run(command, label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--build", action="store_true", help="build locked release binaries; enables macfuse on macOS")
    parser.add_argument("--allow-dirty", action="store_true", help="explicitly permit and record a dirty repository")
    parser.add_argument("--allow-unverified-binaries", action="store_true", help="explicitly permit and record binaries without a build receipt")
    parser.add_argument("--binary-manifest", type=Path, help="local build receipt from a previous --build run")
    parser.add_argument("--min-free-bytes", type=int, default=512 << 20, help="minimum scratch space before tests (default 512 MiB; not a complete disk-use guarantee)")
    parser.add_argument("--application-kind", choices=["application", "game"], default="application", help="game requires an explicit user classification; synthetic fixtures never constitute game evidence")
    parser.add_argument("--playsparse", type=Path)
    parser.add_argument("--io-probe", type=Path)
    parser.add_argument("--stage-timeout", type=int, default=1800, help="seconds per stage, including an optional application (60..86400)")
    parser.add_argument("--source", type=Path, help="optional local application installation")
    parser.add_argument("--executable", type=Path, help="relative executable inside --source")
    parser.add_argument("arguments", nargs=argparse.REMAINDER, help="application arguments after --")
    args = parser.parse_args()
    if not 60 <= args.stage_timeout <= 86400:
        parser.error("--stage-timeout must be between 60 and 86400 seconds")
    if bool(args.source) != bool(args.executable):
        parser.error("--source and --executable must be provided together")
    if args.arguments and not args.source:
        parser.error("application arguments require --source and --executable")
    if not 0 <= args.min_free_bytes <= 1 << 50:
        parser.error("--min-free-bytes must be between zero and 1 PiB")
    repo = Path(__file__).resolve().parent.parent
    try:
        source = args.source.expanduser().resolve(strict=True) if args.source else None
        if source is not None and not source.is_dir():
            raise PrerequisiteMissing("--source must be an application installation directory")
        work = safe_new_work(args.work, repo, source)
    except (OSError, PrerequisiteMissing) as error:
        print(json.dumps({"status": "BLOCKED", "evidence": None, "error": str(error)}))
        return 2
    report = {"version": 2, "status": "FAIL", "commands": [],
              "stages": {name: {"status": "NOT RUN", "evidence": str(work / name / "evidence")}
                         for name in ("readonly", "writable", "adaptive", "tiers")},
              "real_game_validation": "NOT RUN", "game_evidence_gate": "GAME EVIDENCE REQUIRED",
              "application_validation": "NOT RUN", "evidence_type": "native POSIX generated mounted fixtures",
              "evidence": str(work),
              "scope": "generated fixtures on real FUSE/macFUSE mounts; one optional application only"}
    started = time.perf_counter()
    try:
        env = environment(repo, work)
        write_json(work / "environment.json", env)
        identity_before = repository_identity(repo)
        report["repository"] = identity_before
        report["allow_dirty"] = args.allow_dirty
        if identity_before["dirty"] and not args.allow_dirty:
            raise PrerequisiteMissing("Dirty repository; commit/stash changes or explicitly use --allow-dirty")
        report["disk_preflight"] = {"available_bytes": env["available_disk_bytes"], "required_minimum_bytes": args.min_free_bytes}
        if env["available_disk_bytes"] < args.min_free_bytes:
            raise PrerequisiteMissing("Insufficient scratch disk space; choose a larger filesystem or an explicit --min-free-bytes threshold")
        if sys.platform == "linux":
            if not env["fuse_device"].get("opened_read_write") or not env["fuse_helper"]:
                raise PrerequisiteMissing("Linux requires accessible /dev/fuse and fusermount3 or fusermount; no dependencies or privileges were changed")
        elif sys.platform == "darwin":
            if not env["macfuse"]["installed"] or not env["unmount_helper"]:
                raise PrerequisiteMissing("Native macOS requires installed and approved macFUSE; no driver was installed or approved")
        else:
            raise PrerequisiteMissing("This runner supports Linux and native macOS")
        target = Path(os.environ.get("CARGO_TARGET_DIR", str(repo / "target"))).expanduser()
        if not target.is_absolute():
            target = repo / target
        cli = (args.playsparse or target / "release/playsparse").expanduser().resolve()
        probe = (args.io_probe or target / "release/io-probe").expanduser().resolve()
        default_cli, default_probe = ((target / "release" / name).resolve() for name in ("playsparse", "io-probe"))
        manifest = args.binary_manifest or cli.parent / "playsparse-build-provenance.json"
        if args.build and (cli != default_cli or probe != default_probe):
            raise PrerequisiteMissing("--build verifies Cargo output paths; custom binary paths must point to this Cargo target/release directory")
        if args.build:
            if os.environ.get("CARGO_BUILD_TARGET"):
                raise PrerequisiteMissing("Native validation requires CARGO_BUILD_TARGET unset; CARGO_TARGET_DIR remains supported")
            cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
            if not Path(cargo).is_file() or not os.access(cargo, os.X_OK):
                raise PrerequisiteMissing("Rust Cargo executable is unavailable; install the pinned toolchain before --build")
            command = [cargo, "build", "--locked", "--release", "--workspace"]
            if sys.platform == "darwin":
                command.extend(["--features", "macfuse"])
            run(command, "build", repo, work, report, args.stage_timeout)
            write_build_manifest(manifest, repo, {"playsparse": cli, "io-probe": probe}, identity_before, command)
        report["binary_provenance"] = inspect_binary_provenance(
            repo, {"playsparse": cli, "io-probe": probe}, manifest,
            args.allow_dirty, args.allow_unverified_binaries)
        report["binaries"] = report["binary_provenance"]["binaries"]
        doctor_entry = CommandRunner(repo, work, report, 60).run(
            [str(cli), "doctor", "--path", str(work), "--mount-test"], "doctor", allowed_exit_codes=(0, 1, 2))
        doctor = json.loads((work / "doctor.stdout.log").read_text())
        report["mount_backend"] = doctor.get("mount_backend")
        report["mount_diagnostic"] = doctor.get("mount_diagnostic")
        report["mount_preflight"] = doctor.get("mount_test")
        if doctor_entry["exit_code"] == 2:
            raise PrerequisiteMissing(f"Actual mount preflight blocked: {doctor.get('mount_test')}")
        if doctor_entry["exit_code"] or doctor.get("mount_test", {}).get("status") != "PASS":
            raise RuntimeError(f"Actual mount preflight failed: {doctor.get('mount_test')}")
        stages = [("readonly", "mounted-smoke.py", ["--world-bytes", str(10 << 30), "--iterations", "32", "--cache", "64M"]),
                  ("writable", "mounted-update.py", []),
                  ("adaptive", "adaptive-smoke.py", []),
                  ("tiers", "tiered-smoke.py", [])]
        for label, script, extra in stages:
            report["stages"][label] = {"status": "RUNNING", "evidence": str(work / label / "evidence")}
            command = [sys.executable, str(repo / "tools" / script), "--work", str(work / label),
                       "--playsparse", str(cli), "--io-probe", str(probe), *extra]
            run(command, label, repo, work, report, args.stage_timeout)
            stage = json.loads((work / label / "evidence/result.json").read_text())
            report["stages"][label] = {"status": stage.get("status"), "evidence": str(work / label / "evidence")}
            if stage.get("status") != "PASS":
                raise RuntimeError(f"{label} did not report PASS")
        if args.source:
            report["application_validation"] = "RUNNING"
            arguments = args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments
            run([sys.executable, str(repo / "tools/owned-application.py"), "--work", str(work / "application"),
                 "--playsparse", str(cli), "--source", str(source), "--executable", str(args.executable),
                 "--binary-manifest", str(manifest), "--application-kind", args.application_kind,
                 "--timeout", str(args.stage_timeout),
                 *(["--allow-dirty"] if args.allow_dirty else []),
                 *(["--allow-unverified-binaries"] if args.allow_unverified_binaries else []), "--", *arguments],
                "application", repo, work, report, args.stage_timeout)
            application = json.loads((work / "application/evidence/result.json").read_text())
            if application.get("status") != "PASS":
                raise RuntimeError("application did not report PASS")
            report["application_validation"] = "PASS"
            report["real_game_validation"] = application["real_game_validation"]
            report["game_evidence_gate"] = application["game_evidence_gate"]
            report["application_source_hashes"] = {key: application.get(key) for key in ("source_before_sha256", "source_after_sha256", "source_unchanged", "base_unchanged")}
        report["status"] = "PASS"
    except PrerequisiteMissing as error:
        report.update(status="BLOCKED", error=str(error))
    except Exception as error:
        report.update(status="FAIL", error=str(error))
    finally:
        for name, stage in report["stages"].items():
            result_path = work / name / "evidence/result.json"
            if result_path.is_file():
                try:
                    result = json.loads(result_path.read_text())
                    stage["reported_status"] = result.get("status")
                    stage["integrity"] = {key: result.get(key) for key in ("source_unchanged", "base_unchanged", "secondary_unchanged") if key in result}
                except (OSError, ValueError) as error:
                    stage["result_error"] = str(error)
            if stage["status"] == "RUNNING":
                stage["status"] = "FAIL"
        if report["application_validation"] == "RUNNING":
            report["application_validation"] = "FAIL"
        application_path = work / "application/evidence/result.json"
        if application_path.is_file():
            try:
                application = json.loads(application_path.read_text())
                report["application_source_hashes"] = {key: application.get(key) for key in ("source_before_sha256", "source_after_sha256", "source_unchanged", "base_unchanged")}
            except (OSError, ValueError) as error:
                report["application_result_error"] = str(error)
        if "cli" in locals() and cli.is_file():
            # If a stage was interrupted, its daemon may have exited before an
            # ordinary unmount. Only detach this run's observed mountpoints.
            for label in ("readonly", "writable", "adaptive", "tiers", "application"):
                mountpoint = work / label / "mounted"
                try:
                    if mount_present(mountpoint):
                        detach_owned_mount(CommandRunner(repo, work, report), cli, mountpoint, f"cleanup-{label}")
                except Exception as error:
                    report.update(status="FAIL", cleanup_error=str(error))
        if "identity_before" in locals():
            try:
                identity_after = repository_identity(repo)
                report["repository_after"] = identity_after
                report["repository_unchanged_during_run"] = identity_after == identity_before
                if identity_after != identity_before:
                    report.update(status="FAIL", provenance_error="Repository changed during validation; repeat from a stable checkout")
                if "binary_provenance" in report:
                    report["binaries_unchanged_during_run"] = all(
                        digest(value["path"]) == value["sha256"] for value in report["binaries"].values())
                    if not report["binaries_unchanged_during_run"]:
                        report.update(status="FAIL", provenance_error="Runtime binary changed during validation")
            except Exception as error:
                report.update(status="FAIL", provenance_error=str(error))
        if report["status"] != "PASS" and report["real_game_validation"].startswith("PASS"):
            report["real_game_validation"] = "FAIL"
        report["wall_seconds"] = time.perf_counter() - started
        write_json(work / "result.json", report)
        print(json.dumps({"status": report["status"], "evidence": str(work), "error": report.get("error")}, indent=2))
    return 0 if report["status"] == "PASS" else 2 if report["status"] == "BLOCKED" else 1


if __name__ == "__main__":
    with signals():
        sys.exit(main())
