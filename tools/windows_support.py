"""Read-only Windows evidence APIs. Importing this module is portable.

Native calls are made only when WindowsAPI is constructed on Windows. Allocation
counts data streams, including WOF's backing stream, and excludes NTFS metadata.
"""
import ctypes
from contextlib import contextmanager
import hashlib
import json
import math
import os
from pathlib import Path
import random
import signal
import stat
import struct
import sys
import time


class WindowsPrerequisite(RuntimeError):
    pass


@contextmanager
def finalizing(report):
    """Record interruption while allowing owned cleanup and evidence to finish."""
    previous = {}

    def handler(signum, frame):
        report.update(status="FAIL", interruption_during_cleanup=signal.Signals(signum).name)

    for signum in (signal.SIGINT, signal.SIGTERM):
        previous[signum] = signal.signal(signum, handler)
    try:
        yield
    finally:
        for signum, old in previous.items():
            signal.signal(signum, old)


def physical_gate(environment, attested):
    """No absence of a VM hint is itself proof of physical hardware."""
    eligible = bool(
        attested and environment.get("is_windows")
        and environment.get("windows_product_type") == 1
        and environment.get("virtual_machine_detected") is False
    )
    return {
        "status": "NOT RUN" if eligible else "BLOCKED",
        "user_attested_physical_machine": bool(attested),
        "virtual_machine_detected": environment.get("virtual_machine_detected"),
        "evidence_basis": "explicit user attestation, Windows client OS, hardware report and no detected VM hints; not automatic physical proof",
    }


def parse_streams(buffer):
    """Parse bounded FILE_STREAM_INFO without trusting offsets or UTF-16 sizes."""
    result, offset = [], 0
    while True:
        if offset + 24 > len(buffer):
            raise ValueError("truncated FILE_STREAM_INFO")
        next_offset, name_bytes, size, allocated = struct.unpack_from("<IIqq", buffer, offset)
        if name_bytes % 2 or name_bytes > len(buffer) - offset - 24 or size < 0 or allocated < 0:
            raise ValueError("invalid FILE_STREAM_INFO bounds")
        name = buffer[offset + 24:offset + 24 + name_bytes].decode("utf-16-le", errors="strict")
        if not name.startswith(":") or not name.endswith(":$DATA") or "\0" in name:
            raise ValueError("invalid data stream name")
        result.append({"name": name, "logical_bytes": size, "allocated_bytes": allocated})
        if len(result) > 4096:
            raise ValueError("too many file streams")
        if not next_offset:
            return result
        if next_offset % 8 or next_offset < 24 + name_bytes or offset + next_offset >= len(buffer):
            raise ValueError("invalid FILE_STREAM_INFO offset")
        offset += next_offset


class WindowsAPI:
    def __init__(self):
        if sys.platform != "win32":
            raise WindowsPrerequisite("Windows native APIs require Windows")
        try:
            self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            self.wof = ctypes.WinDLL("Wofutil", use_last_error=True)
            self.psapi = ctypes.WinDLL("psapi", use_last_error=True)
        except OSError as error:
            raise WindowsPrerequisite(f"Required Windows evidence API is unavailable: {error}") from error
        pointer, dword, bool32 = ctypes.c_void_p, ctypes.c_uint32, ctypes.c_int32
        self.kernel.CreateFileW.argtypes = [ctypes.c_wchar_p, dword, dword, pointer, dword, dword, pointer]
        self.kernel.CreateFileW.restype = pointer
        self.kernel.CloseHandle.argtypes = [pointer]
        self.kernel.CloseHandle.restype = bool32
        self.kernel.GetFileInformationByHandleEx.argtypes = [pointer, ctypes.c_int32, pointer, dword]
        self.kernel.GetFileInformationByHandleEx.restype = bool32
        self.kernel.GetVolumePathNameW.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, dword]
        self.kernel.GetVolumePathNameW.restype = bool32
        self.kernel.GetVolumeInformationW.argtypes = [ctypes.c_wchar_p, ctypes.c_wchar_p, dword, pointer, pointer, pointer, ctypes.c_wchar_p, dword]
        self.kernel.GetVolumeInformationW.restype = bool32
        self.kernel.GetVolumeInformationByHandleW.argtypes = [pointer, ctypes.c_wchar_p, dword, pointer, pointer, pointer, ctypes.c_wchar_p, dword]
        self.kernel.GetVolumeInformationByHandleW.restype = bool32
        self.kernel.GetDriveTypeW.argtypes = [ctypes.c_wchar_p]
        self.kernel.GetDriveTypeW.restype = dword
        self.kernel.OpenProcess.argtypes = [dword, bool32, dword]
        self.kernel.OpenProcess.restype = pointer
        self.kernel.GetProcessTimes.argtypes = [pointer, pointer, pointer, pointer, pointer]
        self.kernel.GetProcessTimes.restype = bool32
        self.psapi.GetProcessMemoryInfo.argtypes = [pointer, pointer, dword]
        self.psapi.GetProcessMemoryInfo.restype = bool32
        self.wof.WofIsExternalFile.argtypes = [ctypes.c_wchar_p, pointer, pointer, pointer, pointer]
        self.wof.WofIsExternalFile.restype = ctypes.c_int32

    def open(self, path):
        handle = self.kernel.CreateFileW(str(path), 0x80, 7, None, 3, 0x02000000, None)
        if handle == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        return handle

    def streams(self, path):
        handle = self.open(path)
        try:
            size = 4096
            while size <= 1 << 20:
                buffer = ctypes.create_string_buffer(size)
                if self.kernel.GetFileInformationByHandleEx(handle, 7, buffer, size):
                    return parse_streams(buffer.raw)
                error = ctypes.get_last_error()
                if error == 38:  # ERROR_HANDLE_EOF: no directory data streams.
                    return []
                if error not in (122, 234):
                    raise ctypes.WinError(error)
                size *= 2
            raise WindowsPrerequisite("file stream metadata exceeds 1 MiB")
        finally:
            self.kernel.CloseHandle(handle)

    def wof_info(self, path):
        external, provider = ctypes.c_int32(), ctypes.c_uint32()
        info = (ctypes.c_uint32 * 2)()
        length = ctypes.c_uint32(ctypes.sizeof(info))
        status = self.wof.WofIsExternalFile(str(path), ctypes.byref(external), ctypes.byref(provider), info, ctypes.byref(length))
        if status < 0:
            raise OSError(f"WofIsExternalFile failed HRESULT 0x{status & 0xffffffff:08x}")
        if external.value and provider.value == 2 and length.value != ctypes.sizeof(info):
            raise WindowsPrerequisite("unsupported WOF compression metadata version")
        if external.value and provider.value == 2 and info[1] != 0:
            raise WindowsPrerequisite("unsupported nonzero reserved WOF compression flags")
        return {"externally_backed": bool(external.value), "provider": provider.value,
                "algorithm": info[0] if external.value and provider.value == 2 else None,
                "flags": info[1] if external.value and provider.value == 2 else None}

    def volume(self, path):
        root = ctypes.create_unicode_buffer(32768)
        if not self.kernel.GetVolumePathNameW(str(path), root, len(root)):
            raise ctypes.WinError(ctypes.get_last_error())
        name, filesystem = ctypes.create_unicode_buffer(256), ctypes.create_unicode_buffer(256)
        serial, component, flags = ctypes.c_uint32(), ctypes.c_uint32(), ctypes.c_uint32()
        if not self.kernel.GetVolumeInformationW(root.value, name, len(name), ctypes.byref(serial), ctypes.byref(component), ctypes.byref(flags), filesystem, len(filesystem)):
            raise ctypes.WinError(ctypes.get_last_error())
        return {"root": root.value, "serial": serial.value, "filesystem": filesystem.value,
                "flags": flags.value, "drive_type": self.kernel.GetDriveTypeW(root.value)}

    def usage(self, pid):
        class Counters(ctypes.Structure):
            _fields_ = [("cb", ctypes.c_uint32), ("faults", ctypes.c_uint32),
                        *[(name, ctypes.c_size_t) for name in (
                            "peak_working_set", "working_set", "peak_paged", "paged",
                            "peak_nonpaged", "nonpaged", "pagefile", "peak_pagefile")]]
        handle = self.kernel.OpenProcess(0x410, False, pid)
        if not handle:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            values = [ctypes.c_uint64() for _ in range(4)]
            if not self.kernel.GetProcessTimes(handle, *(ctypes.byref(value) for value in values)):
                raise ctypes.WinError(ctypes.get_last_error())
            counters = Counters()
            counters.cb = ctypes.sizeof(counters)
            if not self.psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
                raise ctypes.WinError(ctypes.get_last_error())
            return {"cpu_seconds": (values[2].value + values[3].value) / 10_000_000,
                    "peak_rss_bytes": counters.peak_working_set}
        finally:
            self.kernel.CloseHandle(handle)

    def mount_record(self, path):
        handle = self.open(path)
        try:
            name, filesystem = ctypes.create_unicode_buffer(256), ctypes.create_unicode_buffer(256)
            if not self.kernel.GetVolumeInformationByHandleW(handle, name, len(name), None, None, None, filesystem, len(filesystem)):
                raise ctypes.WinError(ctypes.get_last_error())
            return {"filesystem_type": filesystem.value, "volume_label": name.value,
                    "api": "GetVolumeInformationByHandleW", "real_mount": filesystem.value == "PlaySparse"}
        finally:
            self.kernel.CloseHandle(handle)


def safe_entries(root, api=None):
    """Do not follow links, junctions, placeholders or special files."""
    root = Path(root)
    if not root.is_dir() or root.is_symlink():
        raise ValueError("source must be a regular directory")
    entries = []
    for directory, directories, files in os.walk(root, followlinks=False):
        directories.sort(); files.sort()
        for name in directories + files:
            path = Path(directory) / name
            info = path.lstat()
            reparse = getattr(info, "st_file_attributes", 0) & 0x400
            if stat.S_ISLNK(info.st_mode) or (reparse and getattr(info, "st_reparse_tag", 0) != 0x80000017):
                raise ValueError(f"unsupported link/reparse point: {path}")
            if not stat.S_ISDIR(info.st_mode) and not stat.S_ISREG(info.st_mode):
                raise ValueError(f"unsupported special file: {path}")
            if reparse and (api is None or api.wof_info(path)["provider"] != 2):
                raise ValueError(f"unverified WOF reparse point: {path}")
            entries.append(path)
    return sorted(entries, key=lambda path: path.relative_to(root).as_posix())


def fingerprint(root, api=None):
    """Whole contents + names/type/mode/mtime; allocation and access time excluded."""
    root = Path(root)
    rows, allocation = [], 0
    for path in safe_entries(root, api):
        info = path.stat()
        row = {"path": path.relative_to(root).as_posix(), "directory": path.is_dir(),
               "size": info.st_size if path.is_file() else 0, "mode": info.st_mode,
               "mtime_ns": info.st_mtime_ns}
        if path.is_file():
            digest = hashlib.sha256()
            with path.open("rb") as source:
                while block := source.read(1 << 20):
                    digest.update(block)
            row["sha256"] = digest.hexdigest()
            if api:
                streams = api.streams(path)
                # The source/base fingerprint also covers named stream contents.
                named = []
                for stream in streams:
                    if stream["name"] != "::$DATA":
                        stream_hash = hashlib.sha256()
                        with Path(str(path) + stream["name"][:-6]).open("rb") as payload:
                            while block := payload.read(1 << 20):
                                stream_hash.update(block)
                        named.append({"name": stream["name"], "size": stream["logical_bytes"], "sha256": stream_hash.hexdigest()})
                named.sort(key=lambda stream: stream["name"])
                row["named_streams"] = named
                allocation += sum(stream["allocated_bytes"] for stream in streams)
        rows.append(row)
    canonical = json.dumps(rows, sort_keys=True, separators=(",", ":")).encode()
    return {"definition": "SHA-256 of complete file/stream contents, sorted relative names/type/mode/mtime; allocation and atime excluded",
            "tree_sha256": hashlib.sha256(canonical).hexdigest(), "entries": rows,
            "logical_bytes": sum(row["size"] for row in rows),
            "allocated_data_bytes": allocation if api else None,
            "allocation_definition": "sum FILE_STREAM_INFO.StreamAllocationSize for all file data streams including WOF backing stream; directory/MFT metadata excluded"}


def content_identity(snapshot):
    return [(row["path"], row["directory"], row["size"], row.get("sha256")) for row in snapshot["entries"]]


def query_plan(snapshot, count=300, seed=0x504C4159):
    files = [row for row in snapshot["entries"] if not row["directory"] and row["size"]]
    if not files:
        raise WindowsPrerequisite("comparison needs at least one nonempty regular file")
    if not 1 <= count <= 10000:
        raise ValueError("reads must be 1..10000")
    rng, queries = random.Random(seed), []
    lengths = (4096, 65536, 1048576)
    for index in range(count):
        row = rng.choice(files)
        length = min(row["size"], lengths[index % len(lengths)])
        queries.append({"path": row["path"], "offset": rng.randrange(row["size"] - length + 1), "length": length})
    return queries


def percentiles(values):
    ordered = sorted(values)
    if not ordered:
        raise ValueError("latency samples are empty")
    return {"samples": len(ordered), **{f"p{value}_us": ordered[math.ceil(len(ordered) * value / 100) - 1] / 1000 for value in (50, 95, 99)}}


def read_workload(root, queries, expected=None, usage=None, provider_pid=None):
    """Reads only. Per-read latency excludes verification and file opens."""
    handles, latencies, digests = {}, [], []
    provider_before = usage(provider_pid) if usage and provider_pid else None
    client_cpu, started = time.process_time(), time.perf_counter()
    try:
        for index, query in enumerate(queries):
            name, offset, length = query["path"], query["offset"], query["length"]
            relative = Path(name)
            if relative.is_absolute() or relative.drive or relative.root or ".." in relative.parts or "\\" in name or ":" in name or "\0" in name or not 0 < length <= 16 << 20 or offset < 0:
                raise ValueError("unsafe query")
            if name not in handles:
                handles[name] = (Path(root) / relative).open("rb", buffering=0)
            handle = handles[name]
            before = time.perf_counter_ns()
            handle.seek(offset)
            data = handle.read(length)
            latencies.append(time.perf_counter_ns() - before)
            if len(data) != length:
                raise RuntimeError(f"short read at {name}:{offset}")
            digest = hashlib.sha256(data).hexdigest()
            if expected is not None and digest != expected[index]:
                raise RuntimeError(f"query bytes differ at {name}:{offset}")
            digests.append(digest)
    finally:
        for handle in handles.values():
            handle.close()
    after = usage(provider_pid) if usage and provider_pid else None
    client = usage(os.getpid()) if usage else None
    return {
        "read_latency": percentiles(latencies), "workload_wall_seconds": time.perf_counter() - started,
        "client_cpu_seconds": time.process_time() - client_cpu,
        "client_peak_rss_bytes": client["peak_rss_bytes"] if client else None,
        "provider_cpu_seconds": after["cpu_seconds"] - provider_before["cpu_seconds"] if after else None,
        "provider_peak_rss_bytes": after["peak_rss_bytes"] if after else None,
        "requested_bytes": sum(query["length"] for query in queries), "verified_reads": len(queries),
        "query_digest_sha256": hashlib.sha256("".join(digests).encode()).hexdigest(),
    }, digests
