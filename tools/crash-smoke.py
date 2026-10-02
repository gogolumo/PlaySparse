#!/usr/bin/env python3
"""Linux crash/ENOSPC integration evidence for the atomic store transaction.

LD_PRELOAD stops the actual pack process halfway through a selected write syscall.
The process is killed with SIGKILL. This is fault injection, not a simulated store.
An optional dedicated tiny tmpfs exercises real kernel ENOSPC, never the host disk.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import signal
import subprocess
import sys
import time

HOOK = r'''
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <limits.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static __thread bool inside = false;
ssize_t write(int fd, const void *buffer, size_t length) {
    ssize_t (*real_write)(int,const void*,size_t) = dlsym(RTLD_NEXT,"write");
    if (!real_write) _exit(121);
    if (!inside) {
        inside = true;
        char link[64], path[PATH_MAX];
        snprintf(link,sizeof(link),"/proc/self/fd/%d",fd);
        ssize_t used = readlink(link,path,sizeof(path)-1);
        const char *target = getenv("PLAYSPARSE_PAUSE_WRITE");
        const char *marker = getenv("PLAYSPARSE_PAUSE_MARKER");
        if (used >= 0) path[used] = 0;
        if (used >= 0 && target && marker && strstr(path,target) && length > 1) {
            ssize_t result = real_write(fd,buffer,length/2);
            int marker_fd = open(marker,O_CREAT|O_WRONLY|O_TRUNC,0600);
            if (marker_fd >= 0) { real_write(marker_fd,path,strlen(path)); close(marker_fd); }
            // The harness kills us before this partial write returns to Rust.
            sleep(60);
            inside = false;
            return result;
        }
        inside = false;
    }
    return real_write(fd,buffer,length);
}
'''


def execute(command, evidence, name, expected_success=True, env=None):
    result = subprocess.run(command, capture_output=True, text=True, env=env)
    (evidence / (name + ".stdout.log")).write_text(result.stdout)
    (evidence / (name + ".stderr.log")).write_text(result.stderr)
    if (result.returncode == 0) != expected_success:
        raise RuntimeError(f"unexpected exit for {name}: {result.returncode}: {result.stderr}")
    return {"command": command, "returncode": result.returncode, "stderr": result.stderr}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--playsparse", type=Path, default=Path("target/release/playsparse"))
    parser.add_argument("--disk-full-dir", type=Path, help="Dedicated empty tmpfs <=16 MiB; requires caller to mount it")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("LD_PRELOAD fault injection requires actual Linux; Windows native unit tests are separate")
    binary = str(args.playsparse.resolve(strict=True))
    work = args.work.resolve()
    if work.exists():
        parser.error("work already exists; previous evidence is preserved")
    work.mkdir(parents=True)
    evidence = work / "evidence"
    evidence.mkdir()
    source = work / "source"
    source.mkdir()
    rng = random.Random(104)
    digest = hashlib.sha256()
    with (source / "entropy.dat").open("wb") as stream:
        for _ in range(64):
            block = rng.randbytes(1 << 20)
            stream.write(block)
            digest.update(block)
    report = {"schema": 1, "os": sys.platform, "method": "actual partial write syscall then SIGKILL; real tiny-tmpfs ENOSPC if supplied", "source_sha256": digest.hexdigest(), "status": "FAIL", "cases": [], "disk_full": "NOT_RUN"}
    try:
        cfile = work / "pause-write.c"
        cfile.write_text(HOOK)
        library = work / "pause-write.so"
        execute(["cc", "-shared", "-fPIC", "-Wall", "-Werror", str(cfile), "-ldl", "-o", str(library)], evidence, "compile-hook")
        for name, target in [("kill-object-write", "pack-0000.psp"), ("kill-manifest-write", "manifest.json"), ("kill-commit-write", "COMMITTED.json")]:
            destination = work / name
            marker = work / (name + ".marker")
            env = os.environ.copy()
            env.update({"LD_PRELOAD": str(library), "PLAYSPARSE_PAUSE_WRITE": target, "PLAYSPARSE_PAUSE_MARKER": str(marker)})
            command = [binary, "pack", str(source), str(destination), "--chunker", "fixed"]
            stdout, stderr = evidence / (name + ".stdout.log"), evidence / (name + ".stderr.log")
            with stdout.open("w") as out, stderr.open("w") as err:
                process = subprocess.Popen(command, env=env, stdout=out, stderr=err)
            deadline = time.monotonic() + 30
            while not marker.exists() and process.poll() is None and time.monotonic() < deadline:
                time.sleep(0.002)
            if not marker.exists():
                if process.poll() is None:
                    process.kill()
                    process.wait()
                raise RuntimeError(f"fault pause for {target} was not reached: {stderr.read_text()}")
            process.kill()
            process.wait()
            if process.returncode != -signal.SIGKILL or destination.exists():
                raise RuntimeError("partial transaction was published after SIGKILL")
            rejected = execute([binary, "verify", str(destination)], evidence, name + "-reject", expected_success=False)
            stage = Path(marker.read_text()).parent
            if target == "pack-0000.psp":
                stage = stage.parent
            stage_rejected = execute([binary, "verify", str(stage)], evidence, name + "-stage-reject", expected_success=False)
            report["cases"].append({"name": name, "command": command, "killed_write_path": marker.read_text(), "returncode": process.returncode, "destination_absent": True, "verify_rejects_destination": rejected, "verify_rejects_partial_stage": stage_rejected, "status": "PASS"})
        valid = work / "valid-store"
        report["retry_pack"] = execute([binary, "pack", str(source), str(valid), "--chunker", "fixed"], evidence, "retry-pack")
        report["retry_verify"] = execute([binary, "verify", str(valid)], evidence, "retry-verify")
        for case in ["missing-object", "corrupt-object", "corrupt-manifest", "corrupt-index", "missing-commit"]:
            broken = work / case
            shutil.copytree(valid, broken)
            pack = next((broken / "packs").glob("*.psp"))
            if case == "missing-object":
                pack.unlink()
            elif case == "corrupt-object":
                with pack.open("r+b") as stream:
                    stream.seek(32)
                    byte = stream.read(1)
                    stream.seek(32)
                    stream.write(bytes([byte[0] ^ 0xff]))
            elif case == "corrupt-manifest":
                (broken / "manifest.json").write_text("{broken")
            elif case == "corrupt-index":
                index = broken / "index" / "objects.idx"
                with index.open("r+b") as stream:
                    byte = stream.read(1)
                    stream.seek(0)
                    stream.write(bytes([byte[0] ^ 0xff]))
            else:
                (broken / "COMMITTED.json").unlink()
            rejected = execute([binary, "verify", str(broken)], evidence, case, expected_success=False)
            report["cases"].append({"name": case, "verify": rejected, "status": "PASS"})
        if args.disk_full_dir:
            small = args.disk_full_dir.resolve(strict=True)
            info = [row for row in Path("/proc/self/mountinfo").read_text().splitlines() if row.split()[4] == str(small)]
            if len(info) != 1 or info[0].split(" - ")[1].split()[0] != "tmpfs" or any(small.iterdir()):
                raise RuntimeError("disk-full-dir must be a dedicated empty tmpfs mount")
            stat = os.statvfs(small)
            if stat.f_blocks * stat.f_frsize > 16 << 20:
                raise RuntimeError("refusing to fill tmpfs larger than 16 MiB")
            destination = small / "store"
            rejected = execute([binary, "pack", str(source), str(destination), "--chunker", "fixed"], evidence, "disk-full", expected_success=False)
            if destination.exists() or "space" not in rejected["stderr"].lower():
                raise RuntimeError("expected ENOSPC and absent published store")
            report["disk_full"] = {"status": "PASS", "mountinfo": info[0], "capacity_bytes": stat.f_blocks * stat.f_frsize, "pack": rejected, "destination_absent": True}
        check = hashlib.sha256()
        with (source / "entropy.dat").open("rb") as stream:
            while block := stream.read(1 << 20):
                check.update(block)
        report["source_untouched"] = check.hexdigest() == report["source_sha256"]
        if not report["source_untouched"]:
            raise RuntimeError("source changed")
        report["status"] = "PASS"
    except Exception as error:
        report["error"] = str(error)
    finally:
        (evidence / "result.json").write_text(json.dumps(report, indent=2))
        print(json.dumps({"status": report["status"], "evidence": str(evidence), "error": report.get("error"), "disk_full": report["disk_full"]}, indent=2))
    return 0 if report["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
