#!/usr/bin/env python3
"""Verify local promotion and HTTP object ranges through genuine mounted IO.

Every fixture is generated. Immutable metadata stays separate from promoted
objects and writable overlays. A deliberately corrupt HTTP response must fail
as an ordinary mounted read rather than deliver bytes to the application.
"""
import argparse
from datetime import datetime, timezone
import errno
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import threading
import time
import metadata_validation


spec = importlib.util.spec_from_file_location(
    "mounted_update", Path(__file__).with_name("mounted-update.py")
)
update = importlib.util.module_from_spec(spec)
spec.loader.exec_module(update)
smoke = update.smoke


def write_json(path, value):
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")


class RangeServer:
    def __init__(self, root):
        owner = self
        self.root = root
        self.records = []
        self.lock = threading.Lock()
        self.corrupt = False

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def do_GET(self):
                match = re.fullmatch(r"/packs/(pack-[0-9]+\.psp)", self.path)
                requested = self.headers.get("Range", "")
                bounds = re.fullmatch(r"bytes=([0-9]+)-([0-9]+)", requested)
                if not match or not bounds:
                    self.send_response(400)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                path = owner.root / "packs" / match.group(1)
                if not path.is_file():
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                start, end = map(int, bounds.groups())
                total = path.stat().st_size
                if start > end or end >= total or end - start + 1 > 16 << 20:
                    self.send_response(416)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                with path.open("rb") as stream:
                    stream.seek(start)
                    payload = stream.read(end - start + 1)
                with owner.lock:
                    corrupt = owner.corrupt
                    if corrupt:
                        payload = bytes([payload[0] ^ 0x80]) + payload[1:]
                    owner.records.append({
                        "method": "GET", "path": self.path, "range": requested,
                        "start": start, "end": end, "pack_bytes": total,
                        "returned_bytes": len(payload), "status": 206,
                        "corrupt_response": corrupt,
                    })
                self.send_response(206)
                self.send_header("Content-Range", f"bytes {start}-{end}/{total}")
                self.send_header("Content-Length", str(len(payload)))
                self.send_header("Accept-Ranges", "bytes")
                self.end_headers()
                try:
                    self.wfile.write(payload)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.worker = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def __enter__(self):
        self.worker.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.worker.join(timeout=5)
        if self.worker.is_alive():
            raise RuntimeError("HTTP fixture server did not shut down")


def tier_metrics(mount):
    for event in mount.events:
        if event.get("event") in ("fuse_unmounted", "winfsp_unmounted"):
            metrics = event.get("tiers")
            if metrics is not None:
                return metrics
    raise RuntimeError("mounted tier metrics missing")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--playsparse", type=Path, default=Path("target/release/playsparse"))
    parser.add_argument("--io-probe", type=Path, default=Path("target/release/io-probe"))
    args = parser.parse_args()
    cli = args.playsparse.resolve(strict=True)
    probe = args.io_probe.resolve(strict=True)
    work = args.work.resolve()
    work.mkdir()  # Preserve earlier successes and failures.
    evidence = work / "evidence"
    evidence.mkdir()
    source, secondary, base = (work / name for name in ("source", "secondary", "base"))
    mountpoint = work / "mounted"
    if sys.platform != "win32":
        mountpoint.mkdir()
    repo = Path(__file__).resolve().parent.parent
    git = ["git", "-c", f"safe.directory={repo}", "-C", str(repo)]
    report = {
        "version": 1, "timestamp": datetime.now(timezone.utc).isoformat(),
        "commit": subprocess.check_output(git + ["rev-parse", "HEAD"], text=True).strip(),
        "working_tree_status": subprocess.check_output(git + ["status", "--porcelain"], text=True),
        "os": platform.platform(), "architecture": platform.machine(),
        "binary_sha256": hashlib.sha256(cli.read_bytes()).hexdigest(),
        "status": "FAIL", "commands": [], "scenarios": {}, "configs": {},
        "dataset": "generated TestGame, 4 MiB world; no real game compatibility claim",
        "windows_physical_validation": "BLOCKED_ON_PHYSICAL_WINDOWS",
    }

    def run(command, label):
        return smoke.run_command(command, evidence, label, report["commands"])[0]

    def mount(label, config, overlay, policy=None):
        mounted = update.Mount(
            cli, base, mountpoint, overlay, evidence / f"{label}.trace.jsonl",
            evidence, label, report["commands"],
        )
        mounted.command.extend(["--tiers", str(config)])
        if policy:
            mounted.command.extend(["--policy", str(policy)])
        return mounted

    def check_view(label, mounted, expected, run_probe=False):
        metadata_validation.verify(mountpoint, expected, update.tree(mountpoint), evidence, label, report)
        executable = "testgame.exe" if sys.platform == "win32" else "testgame"
        run([str(mountpoint / executable), "--self-test"], f"{label}-native-executable")
        if run_probe:
            run([
                str(probe), str(source), str(mountpoint), "--iterations", "4",
                "--sequential-limit", str(1 << 20), "--output",
                str(evidence / f"{label}-io-probe.json"),
            ], f"{label}-io-probe")
        return mounted.record

    def summarize(label, mounted, source_name):
        summary = run([
            str(cli), "trace", "summarize", str(evidence / f"{label}.trace.jsonl"),
        ], f"{label}-trace-summary")
        if summary["sources"].get(source_name, 0) == 0:
            raise RuntimeError(f"{label}: trace never reported {source_name}")
        result = {"status": "PASS", "mount_record": mounted.record,
                  "tiers": tier_metrics(mounted), "trace": summary,
                  "unmount_events": mounted.events}
        report["scenarios"][label] = result
        return result

    before = {}
    started = time.perf_counter()
    server = None
    try:
        run([
            sys.executable, str(Path(__file__).with_name("generate-testgame.py")),
            str(source), "--io-probe", str(probe), "--world-bytes", str(4 << 20),
        ], "generate")
        before["source"] = smoke.fingerprint(source)
        run([str(cli), "pack", str(source), str(secondary), "--chunker", "fixed",
             "--chunk-size", "64K"], "pack-secondary")
        run([str(cli), "verify", str(secondary)], "verify-secondary-before")
        base.mkdir()
        (base / "index").mkdir()
        for name in ("manifest.json", "COMMITTED.json", "index/objects.idx"):
            shutil.copy2(secondary / name, base / name)
        before["base"] = smoke.fingerprint(base)
        before["secondary"] = smoke.fingerprint(secondary)
        expected = update.tree(source)

        # Paths in generated configs resolve relative to each config file.
        local_config = work / "secondary-tiers.json"
        write_json(local_config, {"version": 1, "primary_cache": "promoted",
                                  "secondary": "secondary", "promote": True})
        report["configs"][local_config.name] = json.loads(local_config.read_text())
        overlay = work / "overlay"
        local = mount("secondary-promotion", local_config, overlay)
        with local:
            check_view("secondary-promotion", local, expected, run_probe=True)
            (mountpoint / "overlay-created.txt").write_bytes(b"OVERLAY-AND-TIERS\n")
            with (mountpoint / "overlay-created.txt").open("r+b") as stream:
                stream.flush()
                os.fsync(stream.fileno())
            if (mountpoint / "overlay-created.txt").read_bytes() != b"OVERLAY-AND-TIERS\n":
                raise RuntimeError("combined overlay/tier read-after-write mismatch")
        local_result = summarize("secondary-promotion", local, "secondary-local")
        if local_result["tiers"]["promotions"] == 0:
            raise RuntimeError("secondary reads did not promote verified objects")
        expected["overlay-created.txt"] = {
            "kind": "file", "bytes": 18,
            "sha256": hashlib.sha256(b"OVERLAY-AND-TIERS\n").hexdigest(),
        }

        policy = work / "policy.json"
        report["policy"] = run([
            str(cli), "optimize", str(evidence / "secondary-promotion.trace.jsonl"),
            "--output", str(policy),
        ], "optimize-policy")
        promoted_config = work / "promoted-tiers.json"
        write_json(promoted_config, {"version": 1, "primary_cache": "promoted",
                                     "promote": False})
        report["configs"][promoted_config.name] = json.loads(promoted_config.read_text())
        promoted = mount("promoted-remount", promoted_config, overlay, policy)
        with promoted:
            check_view("promoted-remount", promoted, expected)
        promoted_result = summarize("promoted-remount", promoted, "primary-local")
        if promoted_result["tiers"]["primary_cache_hits"] == 0:
            raise RuntimeError("remount did not reuse persistent promoted objects")
        if promoted_result["tiers"]["secondary_hits"] or promoted_result["tiers"]["remote_requests"]:
            raise RuntimeError("offline promoted remount unexpectedly used another tier")

        with RangeServer(secondary) as server:
            remote_config = work / "remote-tiers.json"
            write_json(remote_config, {"version": 1, "promote": False,
                                       "remote": {"base_url": server.url,
                                                  "timeout_ms": 3000, "retries": 0}})
            report["configs"][remote_config.name] = json.loads(remote_config.read_text())
            remote = mount("http-ranges", remote_config, work / "remote-overlay", policy)
            with remote:
                check_view("http-ranges", remote, update.tree(source))
            remote_result = summarize("http-ranges", remote, "remote-http")
            if remote_result["tiers"]["remote_ranges"] == 0:
                raise RuntimeError("mounted HTTP reads did not issue object ranges")

            with server.lock:
                server.corrupt = True
            corrupt = mount("http-corrupt", remote_config, work / "corrupt-overlay")
            with corrupt:
                try:
                    (mountpoint / "readme.txt").read_bytes()
                except OSError as exc:
                    if sys.platform != "win32" and exc.errno != errno.EIO:
                        raise RuntimeError(f"corrupt response returned unexpected errno {exc.errno}") from exc
                    report["scenarios"]["http-corrupt"] = {
                        "status": "PASS", "read_status": "FAIL_EXPECTED",
                        "expected_read_failure": True,
                        "errno": exc.errno, "winerror": getattr(exc, "winerror", None),
                        "mount_record": corrupt.record,
                    }
                else:
                    raise RuntimeError("corrupt remote bytes were delivered to the application")
            corrupt_result = report["scenarios"]["http-corrupt"]
            corrupt_result["tiers"] = tier_metrics(corrupt)
            corrupt_result["unmount_events"] = corrupt.events
            if corrupt_result["tiers"]["errors"] == 0:
                raise RuntimeError("corrupt response failure was not counted")
            corrupt_result["trace"] = run([
                str(cli), "trace", "summarize", str(evidence / "http-corrupt.trace.jsonl"),
            ], "http-corrupt-trace-summary")

        requests = server.records
        if not requests or any(record["start"] < 8 or
                               record["returned_bytes"] >= record["pack_bytes"]
                               for record in requests):
            raise RuntimeError("HTTP fixture observed missing or whole-pack requests")
        report["http"] = {"requests": len(requests),
                          "served_bytes": sum(record["returned_bytes"] for record in requests),
                          "all_requests_exact_object_ranges": True}
        write_json(evidence / "http-requests.json", requests)
        run([str(cli), "verify", str(secondary)], "verify-secondary-after")
        report["clean_unmount"] = not mountpoint.exists() or (
            not os.path.ismount(mountpoint) and not any(mountpoint.iterdir())
        )
        if not report["clean_unmount"]:
            raise RuntimeError("mount did not detach cleanly")
        report["status"] = "PASS"
    except Exception as exc:
        report["error"] = str(exc)
    finally:
        for label, root in (("source", source), ("base", base), ("secondary", secondary)):
            if label in before:
                after = smoke.fingerprint(root)
                unchanged = before[label]["tree_fingerprint_sha256"] == after["tree_fingerprint_sha256"]
                report[f"{label}_unchanged"] = unchanged
                write_json(evidence / f"{label}-before.json", before[label])
                write_json(evidence / f"{label}-after.json", after)
                if not unchanged:
                    report["status"] = "FAIL"
        if server is not None and not (evidence / "http-requests.json").exists():
            write_json(evidence / "http-requests.json", server.records)
        report["wall_seconds"] = time.perf_counter() - started
        write_json(evidence / "result.json", report)
        print(json.dumps({"status": report["status"], "evidence": str(evidence),
                          "error": report.get("error")}, indent=2))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
