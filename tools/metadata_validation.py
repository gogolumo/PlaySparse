"""Content validation with explicit, narrowly scoped macOS metadata accounting.

No source/store fingerprint uses this relaxation. Only extra mounted entries may
be classified; an expected ._* file is always compared as ordinary content.
Layout reference: apple-oss-distributions/xnu, bsd/vfs/vfs_xattr.c.
"""
import hashlib
import ctypes
import os
from pathlib import Path
import struct
import sys


def native_xattrs(path):
    """Darwin's Python builds often omit os.*xattr; use the public libc ABI."""
    library = ctypes.CDLL(None, use_errno=True)
    listing = library.listxattr
    listing.argtypes = [ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
    listing.restype = ctypes.c_ssize_t
    get = library.getxattr
    get.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p,
                   ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    get.restype = ctypes.c_ssize_t
    encoded = os.fsencode(path)

    def checked(size, limit):
        if size < 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))
        if size > limit:
            raise OSError("extended metadata exceeds inspection limit")
        return size

    size = checked(listing(encoded, None, 0, 1), 65536)
    buffer = ctypes.create_string_buffer(size)
    size = checked(listing(encoded, buffer, size, 1), 65536)
    values = {}
    for key in buffer.raw[:size].split(b"\0"):
        if not key:
            continue
        length = checked(get(encoded, key, None, 0, 0, 1), 16 << 20)
        value = ctypes.create_string_buffer(length)
        length = checked(get(encoded, key, value, length, 0, 1), 16 << 20)
        values[os.fsdecode(key)] = value.raw[:length]
    return values


def appledouble(path):
    """Recognize the macOS metadata envelope, not arbitrary dot-underscore data.

    Payload is opaque metadata, retained by hash; this is not an xattr decoder.
    Data forks and unknown entry kinds fail closed. Read only the bounded header.
    """
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        return None
    size = path.stat().st_size
    with path.open("rb") as stream:
        header = stream.read(50)
    if len(header) != 50:
        return None
    magic, version, filler, count = struct.unpack(">II16sH", header[:26])
    if (magic, version, filler, count) != (0x51607, 0x20000, b"Mac OS X        ", 2):
        return None
    entries = [struct.unpack(">III", header[start:start + 12]) for start in (26, 38)]
    if [entry[0] for entry in entries] != [9, 2]:
        return None
    _, finder_offset, finder_length = entries[0]
    _, resource_offset, resource_length = entries[1]
    if (finder_offset != 50 or finder_length < 32
            or resource_offset < finder_offset + finder_length
            or resource_offset + resource_length > size):
        return None
    return {"format": "macos-appledouble-v2", "entries": [
        {"id": kind, "offset": offset, "bytes": length}
        for kind, offset, length in entries
    ], "payload_semantics": "opaque metadata; not application content"}


def xattrs(root, names):
    """Record metadata separately from file bytes; errors are retained explicitly."""
    result = {}
    for name in names:
        path = Path(root) / name
        try:
            attributes = {}
            if hasattr(os, "listxattr"):
                values = {key: os.getxattr(path, key, follow_symlinks=False)
                          for key in os.listxattr(path, follow_symlinks=False)}
            elif sys.platform == "darwin":
                values = native_xattrs(path)
            else:
                raise OSError("xattr inspection unavailable")
            for key, value in sorted(values.items()):
                attributes[key] = {"bytes": len(value), "sha256": hashlib.sha256(value).hexdigest()}
            if attributes:
                result[name] = attributes
        except OSError as error:
            result[name] = {"inspection_error": str(error)}
    return result


def compare(root, expected, actual, platform=None):
    """Return auditable differences; never remove entries from either inventory."""
    platform = sys.platform if platform is None else platform
    missing = sorted(set(expected) - set(actual))
    changed = sorted(name for name in set(expected) & set(actual)
                     if expected[name] != actual[name])
    extra = sorted(set(actual) - set(expected))
    metadata = {}
    unexpected = []
    for name in extra:
        path = Path(name)
        companion = path.with_name(path.name[2:]).as_posix() if path.name.startswith("._") and len(path.name) > 2 else None
        envelope = None
        if (platform == "darwin" and companion in expected and companion in actual
                and actual[name].get("kind") == "file"):
            envelope = appledouble(Path(root) / name)
        if envelope is None:
            unexpected.append(name)
        else:
            metadata[name] = dict(envelope, companion=companion, inventory=actual[name])
    return {"content_matches": not (missing or changed or unexpected),
            "missing": missing, "changed": changed, "unexpected": unexpected,
            "appledouble_sidecars": metadata,
            "xattrs": xattrs(root, sorted(actual)) if platform == "darwin" else {},
            "metadata_policy": "new valid macOS metadata envelopes with expected companions only; expected sidecars remain content; xattrs recorded separately, not claimed preserved"}


def verify(root, expected, actual, evidence, label, report):
    from validation_common import atomic_json
    result = compare(root, expected, actual)
    atomic_json(Path(evidence) / f"{label}-tree.json", {"expected": expected, "actual": actual, "comparison": result})
    report.setdefault("tree_validation", {})[label] = result
    if not result["content_matches"]:
        raise RuntimeError(f"{label}: file content/tree differs; inspect {label}-tree.json")
    return result
