#!/usr/bin/env python3
"""Run genuine Linux FUSE or native macFUSE validation, retaining all evidence.

No dependency installation, privilege escalation or extraction. The work directory
must be new. A generated fixture is never reported as a real game test.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import stat
import subprocess
import sys
import time


class PrerequisiteMissing(RuntimeError):
    pass


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            result.update(block)
    return result.hexdigest()


def optional_text(path):
    try:
        return Path(path).read_text(encoding="utf-8")[:65536].strip()
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
              "uid": os.getuid(), "git": observation(git + ["rev-parse", "HEAD"]),
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
    entry = {"label": label, "command": [str(arg) for arg in command], "status": "RUNNING"}
    report["commands"].append(entry)
    write_json(work / "result.json", report)
    print(f"{label}: running", file=sys.stderr, flush=True)
    started = time.perf_counter()
    with (work / f"{label}.stdout.log").open("w") as out, (work / f"{label}.stderr.log").open("w") as err:
        process = subprocess.Popen(entry["command"], cwd=repo, stdout=out, stderr=err, start_new_session=True)
        try:
            process.wait(timeout=timeout)
        except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
            entry["error"] = type(error).__name__
            # Let existing harnesses run their unmount cleanup before escalation.
            os.killpg(process.pid, signal.SIGINT)
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            raise RuntimeError(f"{label} interrupted; inspect preserved logs") from error
        finally:
            entry.update(exit_code=process.returncode, wall_seconds=time.perf_counter() - started,
                         status="PASS" if process.returncode == 0 and "error" not in entry else "FAIL")
            write_json(work / "result.json", report)
            print(f"{label}: {entry['status']} ({entry['wall_seconds']:.2f}s)", file=sys.stderr, flush=True)
    if process.returncode:
        raise RuntimeError(f"{label} exited {process.returncode}; inspect {work / (label + '.stderr.log')}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--build", action="store_true", help="build locked release binaries; enables macfuse on macOS")
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
    repo = Path(__file__).resolve().parent.parent
    work = args.work.expanduser().resolve()
    if args.source:
        source = args.source.expanduser().resolve(strict=True)
        if not source.is_dir():
            parser.error("--source must be an application installation directory")
        if work.is_relative_to(source) or source.is_relative_to(work):
            parser.error("--work and --source must be separate directory trees")
    work.mkdir(parents=True, exist_ok=False)
    report = {"version": 1, "status": "FAIL", "commands": [], "stages": {},
              "real_game_validation": "NOT RUN", "evidence": str(work),
              "scope": "generated fixtures on real FUSE/macFUSE mounts; one optional application only"}
    started = time.perf_counter()
    try:
        env = environment(repo, work)
        write_json(work / "environment.json", env)
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
        if args.build:
            cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
            command = [cargo, "build", "--locked", "--release", "--workspace"]
            if sys.platform == "darwin":
                command.extend(["--features", "macfuse"])
            run(command, "build", repo, work, report, args.stage_timeout)
        for binary in (cli, probe):
            if not binary.is_file() or not os.access(binary, os.X_OK):
                raise PrerequisiteMissing(f"Executable not available: {binary}; use --build or supply both binary paths")
        report["binaries"] = [{"path": str(binary), "sha256": digest(binary)} for binary in (cli, probe)]
        report["binary_provenance"] = "built by this run" if args.build and not (args.playsparse or args.io_probe) else "prebuilt; hashes recorded, commit correspondence not assumed"
        run([str(cli), "doctor", "--path", str(work)], "doctor", repo, work, report, 30)
        doctor = json.loads((work / "doctor.stdout.log").read_text())
        if not doctor.get("mount_backend", {}).get("available"):
            raise PrerequisiteMissing(f"Binary cannot mount on this machine: {doctor.get('mount_backend')}")
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
            report["real_game_validation"] = "RUNNING for one supplied application"
            arguments = args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments
            run([sys.executable, str(repo / "tools/owned-application.py"), "--work", str(work / "application"),
                 "--playsparse", str(cli), "--source", str(source), "--executable", str(args.executable), "--", *arguments],
                "application", repo, work, report, args.stage_timeout)
            application = json.loads((work / "application/evidence/result.json").read_text())
            if application.get("status") != "PASS":
                raise RuntimeError("application did not report PASS")
            report["real_game_validation"] = "PASS for one supplied application execution; no general game/launcher compatibility claim"
        report["status"] = "PASS"
    except PrerequisiteMissing as error:
        report.update(status="BLOCKED", error=str(error))
    except Exception as error:
        report.update(status="FAIL", error=str(error))
    finally:
        for stage in report["stages"].values():
            if stage["status"] == "RUNNING":
                stage["status"] = "FAIL"
        if report["real_game_validation"].startswith("RUNNING"):
            report["real_game_validation"] = "FAIL for the supplied application; inspect preserved logs"
        if "cli" in locals() and cli.is_file():
            # If a stage was interrupted, its daemon may have exited before an
            # ordinary unmount. Only detach this run's observed mountpoints.
            for label in ("readonly", "writable", "adaptive", "tiers", "application"):
                mountpoint = work / label / "mounted"
                if os.path.ismount(mountpoint):
                    try:
                        run([str(cli), "unmount", str(mountpoint)], f"cleanup-{label}", repo, work, report, 90)
                    except Exception as error:
                        report.update(status="FAIL", cleanup_error=str(error))
        report["wall_seconds"] = time.perf_counter() - started
        write_json(work / "result.json", report)
        print(json.dumps({"status": report["status"], "evidence": str(work), "error": report.get("error")}, indent=2))
    return 0 if report["status"] == "PASS" else 2 if report["status"] == "BLOCKED" else 1


if __name__ == "__main__":
    sys.exit(main())
