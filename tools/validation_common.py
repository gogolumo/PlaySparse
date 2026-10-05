"""Shared validation lifecycle and provenance; no installation or shell commands."""
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import tempfile
import time


class PrerequisiteMissing(RuntimeError):
    pass


class Interrupted(RuntimeError):
    def __init__(self, signum):
        self.signum = signum
        super().__init__(f"interrupted by {signal.Signals(signum).name}")


def atomic_json(path, value):
    """Replace complete evidence atomically; retain the previous report on error."""
    path = Path(path)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=".report-", suffix=".tmp", delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(value, stream, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def digest(path):
    result = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            result.update(block)
    return result.hexdigest()


def safe_new_work(path, repo=None, source=None, sources=()):
    path = Path(path).expanduser()
    if path.is_symlink():
        raise PrerequisiteMissing(f"Refusing a symlink as the work directory: {path}")
    path = path.resolve()
    for label, tree in [("repository", repo), ("source installation", source),
                        *(("source installation", item) for item in sources)]:
        if tree is not None:
            tree = Path(tree).resolve()
            if path.is_relative_to(tree) or tree.is_relative_to(path):
                raise PrerequisiteMissing(f"Work must be outside the {label} and neither tree may contain the other")
    if path.exists() or path.is_symlink():
        raise PrerequisiteMissing(f"Refusing reused/existing work directory; existing evidence is untouched: {path}")
    try:
        path.mkdir(mode=0o700, parents=True, exist_ok=False)
    except FileExistsError as error:
        raise PrerequisiteMissing(f"Work directory was created by another run: {path}") from error
    return path


@contextmanager
def signals():
    """First interrupt requests cleanup; subsequent interrupts do not abort it."""
    old = {}
    interrupted = False

    def handler(signum, frame):
        nonlocal interrupted
        if not interrupted:
            interrupted = True
            raise Interrupted(signum)

    for signum in (signal.SIGINT, signal.SIGTERM):
        old[signum] = signal.signal(signum, handler)
    try:
        yield
    finally:
        for signum, handler_before in old.items():
            signal.signal(signum, handler_before)


def _group_signal(process, signum):
    try:
        os.killpg(process.pid, signum)
        return True
    except ProcessLookupError:
        return False


def stop_process(process, grace=20):
    """Terminate this owned process tree and reap its direct child."""
    if os.name == "nt":
        # No command string: taskkill owns Windows descendant enumeration.
        if process.poll() is None:
            result = subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                                    capture_output=True, timeout=30)
            if result.returncode and process.poll() is None:
                process.kill()
        process.wait(timeout=30)
        return
    _group_signal(process, signal.SIGINT)
    try:
        process.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        _group_signal(process, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            _group_signal(process, signal.SIGKILL)
            process.wait(timeout=5)
    # The direct process may have exited while its descendants remained alive.
    if _group_signal(process, signal.SIGTERM):
        _group_signal(process, signal.SIGKILL)


def mount_present(path):
    """Mount-table lookup still detects a FUSE mount after its daemon dies."""
    target = os.path.abspath(path)

    def unescape(value):
        for encoded, decoded in (("\\040", " "), ("\\011", "\t"), ("\\012", "\n"), ("\\134", "\\")):
            value = value.replace(encoded, decoded)
        return value

    if sys.platform == "linux":
        with Path("/proc/self/mountinfo").open() as stream:
            return any(len(fields := row.split()) > 4 and unescape(fields[4]) == target for row in stream)
    if sys.platform == "darwin":
        result = subprocess.run(["/sbin/mount"], capture_output=True, text=True, timeout=10)
        if result.returncode:
            raise RuntimeError("Cannot inspect macOS mount table")
        return any(row.rsplit(" (", 1)[0].endswith(" on " + target) for row in result.stdout.splitlines())
    if sys.platform == "win32":
        # Directory mounts require querying the resolved directory handle, not
        # os.path.ismount's drive-root predicate. Reuse the native tested probe.
        spec = importlib.util.spec_from_file_location("validation_mount_probe", Path(__file__).with_name("mounted-smoke.py"))
        probe = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(probe)
        record = probe.windows_mount_record(Path(path))
        return bool(record and record.get("real_mount"))
    return os.path.ismount(path)


def detach_owned_mount(runner, cli, path, label):
    if not mount_present(path):
        return
    try:
        runner.run([str(cli), "unmount", str(path)], label, timeout=90)
    except Exception:
        # A disconnected FUSE root may defeat CLI canonicalization. Preserve
        # that failure, then use the ordinary OS helper on the exact owned path.
        if sys.platform == "linux":
            helper = shutil.which("fusermount3") or shutil.which("fusermount")
            if helper is None:
                raise
            command = [helper, "-u", "--", str(path)]
        elif sys.platform == "darwin":
            command = ["/sbin/umount", str(path)]
        else:
            raise
        runner.run(command, label + "-os-helper", timeout=90)
    if mount_present(path):
        raise RuntimeError(f"Mount remains attached after cleanup: {path}")


class CommandRunner:
    def __init__(self, repo, evidence, report, timeout=1800, grace=20):
        self.repo, self.evidence, self.report = Path(repo), Path(evidence), report
        self.timeout, self.grace = timeout, grace

    def run(self, argv, label, cwd=None, timeout=None, env=None, allowed_exit_codes=(0,)):
        if not label or any(char not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_" for char in label):
            raise ValueError("Invalid command evidence label")
        entry = {"label": label, "command": [str(arg) for arg in argv],
                 "cwd": str(cwd or self.repo), "timestamp": datetime.now(timezone.utc).isoformat(),
                 "status": "RUNNING", "exit_code": None}
        self.report.setdefault("commands", []).append(entry)
        atomic_json(self.evidence / "result.json", self.report)
        started, process = time.perf_counter(), None
        try:
            with (self.evidence / f"{label}.stdout.log").open("w") as out, (self.evidence / f"{label}.stderr.log").open("w") as err:
                process = subprocess.Popen(entry["command"], cwd=cwd or self.repo, stdout=out, stderr=err,
                                           start_new_session=os.name != "nt", env=env)
                try:
                    process.wait(timeout=timeout or self.timeout)
                except (subprocess.TimeoutExpired, Interrupted, KeyboardInterrupt):
                    stop_process(process, self.grace)
                    raise
                if process.returncode not in allowed_exit_codes:
                    raise RuntimeError(f"{label} exited {process.returncode}; inspect preserved logs")
                if os.name != "nt" and _group_signal(process, 0):
                    stop_process(process, self.grace)
                    raise RuntimeError(f"{label} left child processes running; the owned group was terminated")
            entry["status"] = "PASS" if process.returncode == 0 else "BLOCKED" if process.returncode == 2 else "FAIL"
            return entry
        except BaseException as error:
            entry.update(status="FAIL", error=str(error), failure_type=type(error).__name__)
            if process is not None and (process.poll() is None or (os.name != "nt" and _group_signal(process, 0))):
                try:
                    stop_process(process, self.grace)
                except Exception as cleanup_error:
                    entry["cleanup_error"] = str(cleanup_error)
            raise
        finally:
            entry.update(exit_code=process.returncode if process is not None else None,
                         wall_seconds=time.perf_counter() - started)
            atomic_json(self.evidence / "result.json", self.report)


def repository_identity(repo):
    repo = Path(repo).resolve()
    git = ["git", "-c", f"safe.directory={repo}", "-C", str(repo)]

    def read(arguments):
        result = subprocess.run(git + arguments, capture_output=True, timeout=30)
        if result.returncode:
            raise PrerequisiteMissing("Cannot identify repository revision; Git metadata is required")
        if len(result.stdout) > 4 << 20:
            raise PrerequisiteMissing("Repository identity exceeds the bounded 4 MiB file-list/status limit")
        return result.stdout

    sha = read(["rev-parse", "HEAD"]).decode().strip()
    status = read(["status", "--porcelain"]).decode("utf-8", errors="replace")
    files = sorted(set(read(["ls-files", "--cached", "--others", "--exclude-standard", "-z"]).split(b"\0")) - {b""})
    if len(files) > 100000:
        raise PrerequisiteMissing("Repository identity exceeds 100000 files")
    identity = hashlib.sha256()
    for encoded in files:
        relative = os.fsdecode(encoded)
        path = repo / relative
        identity.update(encoded + b"\0")
        if path.is_symlink():
            identity.update(b"symlink\0" + os.fsencode(os.readlink(path)))
        elif path.is_file():
            identity.update(str(path.stat().st_mode & 0o777).encode() + b"\0")
            identity.update(digest(path).encode())
        else:
            identity.update(b"missing")
    return {"git_sha": sha, "dirty": bool(status), "working_tree_status": status,
            "source_digest_sha256": identity.hexdigest(), "source_digest_definition":
            "SHA-256 of sorted Git tracked and nonignored untracked filenames, modes and file SHA-256s; symlinks record link text"}


def write_build_manifest(path, repo, binaries, identity_before, command):
    after = repository_identity(repo)
    if after != identity_before:
        raise PrerequisiteMissing("Repository changed during the build; rebuild from a stable checkout")
    value = {"version": 1, "repository": after, "command": [str(arg) for arg in command],
             "timestamp": datetime.now(timezone.utc).isoformat(),
             "binaries": {role: {"path": str(Path(binary).resolve()), "sha256": digest(binary)}
                          for role, binary in binaries.items()},
             "basis": "successful validation-runner build with unchanged repository identity; local receipt, not a signed attestation"}
    atomic_json(path, value)
    return value


def inspect_binary_provenance(repo, binaries, manifest=None, allow_dirty=False, allow_unverified=False):
    identity = repository_identity(repo)
    if identity["dirty"] and not allow_dirty:
        raise PrerequisiteMissing("Dirty repository; commit/stash changes or explicitly use --allow-dirty")
    hashes = {}
    for role, binary in binaries.items():
        binary = Path(binary).resolve()
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise PrerequisiteMissing(f"Missing executable {binary}; use --build or supply valid binary paths")
        hashes[role] = {"path": str(binary), "sha256": digest(binary)}
    result = {"repository": identity, "binaries": hashes, "allow_dirty": allow_dirty,
              "allow_unverified_binaries": allow_unverified, "status": "UNVERIFIED"}
    if manifest is not None and Path(manifest).is_file():
        with Path(manifest).open("rb") as stream:
            data = stream.read((1 << 20) + 1)
        if len(data) > 1 << 20:
            raise PrerequisiteMissing("Binary build receipt exceeds 1 MiB")
        try:
            receipt = json.loads(data)
            if receipt["version"] != 1 or receipt["repository"] != identity:
                raise PrerequisiteMissing("Binary/repository revision or source digest mismatch; rebuild binaries")
            for role, value in hashes.items():
                if receipt["binaries"][role]["sha256"] != value["sha256"]:
                    raise PrerequisiteMissing(f"Binary hash mismatch for {role}; rebuild binaries")
        except (KeyError, TypeError, ValueError) as error:
            raise PrerequisiteMissing("Invalid binary build receipt") from error
        result.update(status="VERIFIED_LOCAL_BUILD_RECEIPT", manifest=str(Path(manifest).resolve()))
    elif not allow_unverified:
        raise PrerequisiteMissing("No matching binary build receipt; use --build, --binary-manifest, or explicitly --allow-unverified-binaries")
    return result
