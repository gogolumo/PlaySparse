#!/usr/bin/env python3
"""Opt-in real WKWebView/IPC acceptance on a Mac with an approved macFUSE driver.

Generated fixture only. Failure retains the running application for safe runtime
inspection; this tool never force-unmounts or kills arbitrary game processes.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def identity(source):
    return {
        p.relative_to(source).as_posix(): {
            "sha256": hashlib.sha256(p.read_bytes()).hexdigest(),
            "size": p.stat().st_size,
            "mode": p.stat().st_mode & 0o777,
        }
        for p in sorted(source.rglob("*")) if p.is_file()
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--keep-open", action="store_true")
    args = parser.parse_args()
    if sys.platform != "darwin":
        raise SystemExit("This WKWebView acceptance harness targets native macOS. Use portable service tests elsewhere.")
    root = Path(__file__).resolve().parents[1]
    work = args.work.resolve()
    if work.exists():
        raise SystemExit("Acceptance work directory must be new; existing evidence is retained.")
    work.mkdir(parents=True)
    source = work / "Generated Game"
    (source / "assets").mkdir(parents=True)
    content = b"PlaySparse generated native UI fixture.\n" * 200000
    (source / "assets/data.bin").write_bytes(content)
    (source / "assets/duplicate.bin").write_bytes(content)
    (source / ".playsparse-generated-fixture").write_bytes(b"desktop-acceptance-v1")
    launcher = source / "fixture-game"
    launcher.write_text("#!/bin/sh\nexec sleep 30\n")
    launcher.chmod(0o755)
    original = identity(source)
    if not args.skip_build:
        subprocess.run(["npm", "run", "tauri", "build", "--", "--debug", "--no-bundle", "--features", "acceptance"], cwd=root / "desktop", check=True)
    env = {**os.environ, "PLAYSPARSE_DESKTOP_DATA_DIR": str(work / "library"), "PLAYSPARSE_DESKTOP_ACCEPTANCE_SOURCE": str(source)}
    app = root / "desktop/src-tauri/target/debug/playsparse-app"
    receipt = work / "library/native-acceptance.json"
    with (work / "application.log").open("wb") as log:
        process = subprocess.Popen([str(app)], env=env, stdout=log, stderr=log)
    print(f"Native acceptance application PID {process.pid}; evidence: {work}", flush=True)
    deadline = time.monotonic() + 180
    while not receipt.exists() and process.poll() is None and time.monotonic() < deadline:
        time.sleep(1)
    if not receipt.exists():
        raise SystemExit(f"No completion receipt; inspect {work}. Running application retained if alive.")
    result = json.loads(receipt.read_text())
    result["original_identity"] = original
    result["source_unchanged"] = original == identity(source)
    result["application_sha256"] = hashlib.sha256(app.read_bytes()).hexdigest()
    result["tested_commit"] = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    result["working_tree_dirty"] = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=root, text=True).strip())
    result["platform"] = "native macOS; generated fixture, not commercial-game compatibility"
    if not result["source_unchanged"]:
        result["status"] = "FAIL"
    (work / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    if result["status"] != "PASS":
        raise SystemExit(f"FAIL; inspect {work}. Application retained for safe recovery.")
    # Capture only this application's window, never an unrelated full desktop.
    time.sleep(2)
    script = f'import CoreGraphics; let rows = CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as? [[String:Any]] ?? []; for row in rows {{ if row[kCGWindowOwnerPID as String] as? Int == {process.pid}, row[kCGWindowLayer as String] as? Int == 0 {{ print(row[kCGWindowNumber as String] as! Int); break }} }}'
    window = subprocess.check_output(["swift", "-e", script], text=True).strip()
    if window:
        subprocess.run(["screencapture", "-x", "-l", window, str(work / "native-library.png")], check=True)
    if not args.keep_open:
        # Final verified state has no active jobs, processes or mounts.
        process.terminate()
        process.wait(timeout=15)
    print(f"PASS: real native navigation, register, analyze, pack, verify, mount, exact bytes, overlay, launch, stop, unmount; source unchanged. {work / 'result.json'}")


if __name__ == "__main__":
    main()
