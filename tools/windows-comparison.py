#!/usr/bin/env python3
"""Verified original / disposable WOF copy / mounted PlaySparse warm comparison.

The original is opened for reads only. compact /EXE touches only a newly created
copy outside Git. No CompactOS, driver installation or global cache dropping is
performed. A runtime PASS is separate from physical Windows evidence.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

import validation_common as common
from windows_support import (
    WindowsAPI, WindowsPrerequisite, content_identity, finalizing, fingerprint, physical_gate,
    query_plan, read_workload, safe_entries,
)

ALGORITHMS = {"XPRESS4K": 0, "LZX": 1, "XPRESS8K": 2, "XPRESS16K": 3}


def copy_disposable(source, destination, api=None):
    """Independent regular-file payloads; no hardlinks or source compression."""
    source, destination = Path(source), Path(destination)
    if destination.is_relative_to(source) or source.is_relative_to(destination):
        raise ValueError("copy destination and source must be separate")
    entries = safe_entries(source, api)
    destination.mkdir()
    for path in entries:
        target = destination / path.relative_to(source)
        if path.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            with path.open("rb") as original, target.open("xb") as copy:
                shutil.copyfileobj(original, copy, 1 << 20)
            shutil.copystat(path, target, follow_symlinks=False)
            # Compression may require write permission on this disposable copy.
            target.chmod(target.stat().st_mode | 0o200)


def compact_command(destination, algorithm):
    if algorithm not in ALGORITHMS:
        raise ValueError("unsupported WOF algorithm")
    system = Path(os.environ.get("SystemRoot", r"C:\Windows"))
    return [str(system / "System32" / "compact.exe"), "/C", "/A", "/F", "/Q",
            f"/EXE:{algorithm}", f"/S:{Path(destination)}", "*"]


def verified_wof(root, api, algorithm):
    files, skipped = [], []
    for path in safe_entries(root, api):
        if not path.is_file():
            continue
        info = api.wof_info(path)
        row = {"path": path.relative_to(root).as_posix(), **info}
        if info["externally_backed"]:
            if info["provider"] != 2 or info["algorithm"] != ALGORITHMS[algorithm]:
                raise RuntimeError("compact produced an unexpected provider/algorithm")
            files.append(row)
        else:
            skipped.append(row)
    if not files:
        raise WindowsPrerequisite("compact produced no verified WOF_PROVIDER_FILE objects; no WOF result may be claimed")
    return {"status": "VERIFIED", "api": "WofIsExternalFile", "algorithm": algorithm,
            "wof_files": files, "uncompressed_files": skipped,
            "note": "zero-length/ineligible files can remain ordinary files; all counts are reported"}


def bounded_json(path, maximum=4 << 20):
    with Path(path).open("rb") as stream:
        payload = stream.read(maximum + 1)
    if len(payload) > maximum:
        raise ValueError("comparison input JSON exceeds limit")
    return json.loads(payload)


class ReadOnlyMount:
    def __init__(self, cli, store, mountpoint, evidence, runner, report, label, api, cache):
        self.cli, self.store, self.path, self.evidence = cli, store, mountpoint, evidence
        self.runner, self.report, self.label, self.api, self.cache = runner, report, label, api, cache
        self.process, self.logs, self.metrics = None, [], None

    def __enter__(self):
        if self.path.exists():
            raise RuntimeError("comparison mountpoint already exists; it is never reused blindly")
        command = [str(self.cli), "mount", str(self.store), str(self.path), "--cache", self.cache]
        self.entry = {"label": self.label, "command": command, "status": "RUNNING", "kind": "read-only mount"}
        self.report["commands"].append(self.entry)
        self.started = time.perf_counter()
        try:
            self.logs = [(self.evidence / f"{self.label}.{kind}.log").open("w") for kind in ("stdout", "stderr")]
            self.process = subprocess.Popen(command, stdout=self.logs[0], stderr=self.logs[1], cwd=self.runner.repo)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    raise RuntimeError("WinFsp mount host exited before readiness")
                if self.path.exists():
                    self.record = self.api.mount_record(self.path)
                    if self.record["real_mount"]:
                        return self
                time.sleep(0.05)
            raise RuntimeError("OS-verified WinFsp mount did not become ready within 30 seconds")
        except BaseException:
            self.close(failed=True)
            raise

    def close(self, failed=False):
        errors = []
        try:
            # A provider can exit before its directory volume is detached.
            # This is exclusively the new mountpoint created by this instance.
            if self.path.exists():
                self.runner.run([str(self.cli), "unmount", str(self.path)], self.label + "-unmount", timeout=60)
            if self.process is not None and self.process.poll() is None:
                self.process.wait(timeout=60)
        except BaseException as error:
            errors.append(str(error))
            if self.process is not None:
                try:
                    common.stop_process(self.process)
                except Exception as cleanup_error:
                    errors.append(str(cleanup_error))
        finally:
            for log in self.logs:
                log.close()
        if self.process is not None:
            self.entry["exit_code"] = self.process.returncode
            if self.process.returncode != 0:
                errors.append("mount host did not exit successfully")
        self.entry.update(status="FAIL" if failed or errors else "PASS", wall_seconds=time.perf_counter() - self.started)
        stderr = self.evidence / f"{self.label}.stderr.log"
        if stderr.is_file():
            with stderr.open("rb") as stream:
                payload = stream.read((1 << 20) + 1)
            if len(payload) > 1 << 20:
                errors.append("mount metrics log exceeds the bounded 1 MiB limit")
            for line in payload[:1 << 20].decode(errors="replace").splitlines():
                try:
                    event = json.loads(line)
                except ValueError:
                    continue
                if event.get("event") == "winfsp_unmounted":
                    self.metrics = event
        if not failed and (self.metrics is None or self.metrics.get("read_errors", 0)):
            errors.append("successful unmount metrics missing or read errors reported")
        try:
            if self.path.exists() and (self.api.mount_record(self.path)["real_mount"] or any(self.path.iterdir())):
                errors.append("mount remains attached or materialized files appeared")
        except Exception as error:
            errors.append(f"could not verify owned mount teardown: {error}")
        if errors:
            self.entry["status"] = "FAIL"
            self.entry["cleanup_errors"] = errors
            raise RuntimeError("; ".join(errors))

    def __exit__(self, kind, value, traceback):
        self.close(failed=kind is not None)


def worker(root, queries_path, expected_path, provider_pid=None):
    api = WindowsAPI()
    queries, expected = bounded_json(queries_path), bounded_json(expected_path)
    if not isinstance(queries, list) or not 1 <= len(queries) <= 10000 or len(expected) != len(queries):
        raise ValueError("invalid query/expected count")
    # Explicit preload, then identical timed replay. Kernel/device caches retained.
    read_workload(root, queries, expected=expected)
    result, _ = read_workload(root, queries, expected=expected, usage=api.usage, provider_pid=provider_pid)
    result["cache_state"] = "warm requested ranges: one explicit identical preload; OS/device caches retained and uncontrolled; not disk-cold"
    result["client_memory_scope"] = "fresh worker process lifetime peak including preload; excludes parent fingerprint/copy/pack allocations"
    print(json.dumps(result))


def run_comparison(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--playsparse", type=Path)
    parser.add_argument("--binary-manifest", type=Path)
    parser.add_argument("--environment", type=Path)
    parser.add_argument("--physical-machine", action="store_true")
    parser.add_argument("--allow-dirty", action="store_true")
    parser.add_argument("--allow-unverified-binaries", action="store_true")
    parser.add_argument("--algorithm", choices=tuple(ALGORITHMS), default="XPRESS4K")
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--reads", type=int, default=300)
    parser.add_argument("--cache", default="64M")
    parser.add_argument("--timeout", type=int, default=1800)
    parser.add_argument("--worker-root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--worker-queries", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--worker-expected", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--provider-pid", type=int, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if args.worker_root:
        worker(args.worker_root, args.worker_queries, args.worker_expected, args.provider_pid)
        return 0
    if not args.work or not args.source or not args.playsparse:
        parser.error("--work, --source and --playsparse are required")
    repo, work = Path(__file__).resolve().parent.parent, None
    source, base, before_source, before_base = args.source.resolve(), None, None, None
    cli, api, runner, mount = None, None, None, None
    report = {"schema": 1, "status": "FAIL", "timestamp": datetime.now(timezone.utc).isoformat(),
              "commands": [], "trials": [], "wof_validation": "NOT RUN", "physical_wof_validation": "NOT RUN",
              "scope": "file-system read workload; no game/application startup or launcher execution",
              "cache_state": "repeated warm trials; no disk-cold claim", "application_startup": "NOT RUN",
              "read_latency_definition": "seek + read only; file opens and SHA-256 comparison excluded; wall/CPU include verification",
              "cpu_definition": "client process user+kernel CPU and WinFsp process CPU; not total system/driver CPU",
              "read_amplification_definition": "PlaySparse raw loaded bytes / driver returned bytes for preload+timed replay; WOF/native kernel amplification unmeasured/null",
              "allocation_definition": "all file data streams, including WOF backing data; directory and filesystem metadata excluded"}
    try:
        work = common.safe_new_work(args.work, repo=repo, source=source)
        environment = bounded_json(args.environment, 1 << 20) if args.environment else {"is_windows": sys.platform == "win32", "virtual_machine_detected": None}
        report["environment"] = environment
        report["physical_wof_validation"] = physical_gate(environment, args.physical_machine)
        report["repository"] = common.repository_identity(repo)
        if sys.platform != "win32":
            raise common.PrerequisiteMissing("WOF comparison requires native Windows; no Windows result is produced on this host")
        if not 3 <= args.trials <= 10 or not 1 <= args.reads <= 10000 or not 60 <= args.timeout <= 86400:
            raise common.PrerequisiteMissing("trials must be 3..10, reads 1..10000 and timeout 60..86400")
        if args.physical_machine and report["physical_wof_validation"]["status"] == "BLOCKED":
            raise common.PrerequisiteMissing("physical comparison needs the recorded client Windows environment and explicit attestation")
        report["provenance"] = common.inspect_binary_provenance(repo, {"playsparse": args.playsparse}, args.binary_manifest, args.allow_dirty, args.allow_unverified_binaries)
        cli, api = args.playsparse.resolve(strict=True), WindowsAPI()
        report["source_volume"], report["work_volume"] = api.volume(source), api.volume(work)
        if report["work_volume"]["filesystem"] != "NTFS" or report["work_volume"]["drive_type"] != 3:
            raise common.PrerequisiteMissing("WOF disposable copy requires local fixed NTFS storage")
        if (report["source_volume"]["root"], report["source_volume"]["serial"]) != (report["work_volume"]["root"], report["work_volume"]["serial"]):
            raise common.PrerequisiteMissing("source and evidence must share the same volume for a storage comparison")
        before_source = fingerprint(source, api)
        common.atomic_json(work / "source-before.json", before_source)
        for row in before_source["entries"]:
            for stream in row.get("named_streams", []):
                if stream["name"].casefold() != ":wofcompresseddata:$data":
                    raise common.PrerequisiteMissing("application data streams are unsupported by CAS; comparison rejects them rather than silently dropping them")
        needed = before_source["logical_bytes"] * 2 + (1 << 30)
        available = shutil.disk_usage(work).free
        report["disk_space"] = {"available_bytes": available, "required_bytes": needed}
        if available < needed:
            raise common.PrerequisiteMissing("insufficient space for full disposable copy and conservative new-store budget")
        runner = common.CommandRunner(repo, work, report, timeout=args.timeout)
        copy, base, mount = work / "wof-copy", work / "base", work / "mounted"
        copy_disposable(source, copy, api)
        if content_identity(before_source) != content_identity(fingerprint(copy, api)):
            raise RuntimeError("disposable copy differs before compression")
        runner.run(compact_command(copy, args.algorithm), "compact", cwd=copy)
        report["wof_validation"] = verified_wof(copy, api, args.algorithm)
        copy_snapshot = fingerprint(copy, api)
        if content_identity(before_source) != content_identity(copy_snapshot):
            raise RuntimeError("WOF logical bytes differ after compression")
        common.atomic_json(work / "wof-copy.json", copy_snapshot)
        runner.run([str(cli), "pack", str(source), str(base)], "pack")
        before_base = fingerprint(base, api)
        common.atomic_json(work / "base-before.json", before_base)
        runner.run([str(cli), "verify", str(base)], "verify")
        report["storage"] = {"original": {key: before_source[key] for key in ("logical_bytes", "allocated_data_bytes")},
                             "wof": {key: copy_snapshot[key] for key in ("logical_bytes", "allocated_data_bytes")},
                             "playsparse": {"logical_bytes": before_source["logical_bytes"], "encoded_bytes": before_base["logical_bytes"], "allocated_data_bytes": before_base["allocated_data_bytes"]}}
        queries = query_plan(before_source, args.reads)
        _, expected = read_workload(source, queries)
        common.atomic_json(work / "queries.json", queries)
        common.atomic_json(work / "expected.json", expected)
        for trial in range(args.trials):
            order = ("original", "wof", "playsparse")
            order = order[trial % 3:] + order[:trial % 3]
            row = {"trial": trial, "order": order, "modes": {}}
            report["trials"].append(row)
            for mode in order:
                label = f"trial-{trial}-{mode}"
                def measure(path, provider=None):
                    command = [sys.executable, str(Path(__file__).resolve()), "--worker-root", str(path),
                               "--worker-queries", str(work / "queries.json"), "--worker-expected", str(work / "expected.json")]
                    if provider:
                        command.extend(["--provider-pid", str(provider)])
                    runner.run(command, label)
                    return bounded_json(work / f"{label}.stdout.log")
                if mode == "playsparse":
                    with ReadOnlyMount(cli, base, mount, work, runner, report, label + "-mount", api, args.cache) as mounted:
                        measured = measure(mount, mounted.process.pid)
                        measured["mount_record"] = mounted.record
                    measured["provider_metrics"] = mounted.metrics
                    returned = mounted.metrics.get("returned_bytes", 0)
                    measured["read_amplification"] = mounted.metrics["cache"]["raw_bytes_loaded"] / returned if returned else None
                else:
                    measured = measure(source if mode == "original" else copy)
                    measured["read_amplification"] = None
                row["modes"][mode] = measured
            if len({value["query_digest_sha256"] for value in row["modes"].values()}) != 1:
                raise RuntimeError("comparison query hashes disagree")
            common.atomic_json(work / "result.json", report)
        report["status"] = "PASS"
        if report["physical_wof_validation"]["status"] != "BLOCKED":
            report["physical_wof_validation"]["status"] = "PASS"
    except (common.PrerequisiteMissing, WindowsPrerequisite) as error:
        report.update(status="BLOCKED", error=str(error))
    except BaseException as error:
        report.update(status="FAIL", error=str(error), failure_type=type(error).__name__)
    finally:
        with finalizing(report):
            if mount is not None and api is not None and runner is not None:
                try:
                    if mount.exists() and api.mount_record(mount)["real_mount"]:
                        runner.run([str(cli), "unmount", str(mount)], "cleanup-unmount", timeout=60)
                    if mount.exists() and api.mount_record(mount)["real_mount"]:
                        raise RuntimeError("owned WOF comparison mount remains attached")
                    report["cleanup"] = {"status": "PASS", "mountpoint": str(mount)}
                except Exception as error:
                    report.update(status="FAIL", cleanup_error=str(error),
                                  cleanup={"status": "FAIL", "mountpoint": str(mount)})
            for label, root, before in (("source", source, before_source), ("base", base, before_base)):
                if before is None:
                    continue
                try:
                    after = fingerprint(root, WindowsAPI())
                    common.atomic_json(work / f"{label}-after.json", after)
                    report[f"{label}_unchanged"] = after["tree_sha256"] == before["tree_sha256"]
                    if not report[f"{label}_unchanged"]:
                        report.update(status="FAIL", error=f"immutable {label} changed")
                except Exception as error:
                    report.update(status="FAIL", integrity_error=str(error))
            if work:
                if report["status"] != "PASS" and report["physical_wof_validation"] != "NOT RUN":
                    if report["physical_wof_validation"]["status"] == "PASS":
                        report["physical_wof_validation"]["status"] = "FAIL"
                common.atomic_json(work / "result.json", report)
            print(json.dumps({"status": report["status"], "evidence": str(work) if work else None, "error": report.get("error")}, indent=2))
    return {"PASS": 0, "FAIL": 1, "BLOCKED": 2}[report["status"]]


def main(argv=None):
    with common.signals():
        return run_comparison(argv)


if __name__ == "__main__":
    sys.exit(main())
