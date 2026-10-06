"""Portable metadata-model tests; no mocked mount is hardware evidence."""
import importlib.util
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import metadata_validation as metadata

spec = importlib.util.spec_from_file_location("update_metadata_test", Path(__file__).resolve().parents[1] / "tools/mounted-update.py")
update = importlib.util.module_from_spec(spec)
spec.loader.exec_module(update)


class MetadataTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "file").write_bytes(b"application bytes")
        self.expected = update.tree(self.root)

    def tearDown(self):
        self.temp.cleanup()

    def sidecar(self, name="._file", kind=9):
        data = (struct.pack(">II16sH", 0x51607, 0x20000, b"Mac OS X        ", 2)
                + struct.pack(">III", kind, 50, 32)
                + struct.pack(">III", 2, 82, 0) + bytes(32))
        (self.root / name).write_bytes(data)

    def compare(self, platform="darwin"):
        return metadata.compare(self.root, self.expected, update.tree(self.root), platform)

    def test_new_envelope_is_classified_and_raw_inventory_is_retained(self):
        self.sidecar()
        result = self.compare()
        self.assertTrue(result["content_matches"])
        self.assertEqual(result["appledouble_sidecars"]["._file"]["companion"], "file")
        self.assertIn("sha256", result["appledouble_sidecars"]["._file"]["inventory"])

    def test_linux_and_windows_remain_strict(self):
        self.sidecar()
        for platform in ("linux", "win32"):
            self.assertFalse(self.compare(platform)["content_matches"])

    def test_application_dot_underscore_file_is_never_exempt(self):
        self.sidecar()
        self.expected = update.tree(self.root)
        (self.root / "._file").write_bytes(b"changed")
        self.assertEqual(self.compare()["changed"], ["._file"])
        (self.root / "._file").unlink()
        self.assertEqual(self.compare()["missing"], ["._file"])

    def test_invalid_or_orphan_or_data_fork_is_rejected(self):
        for name, kind in (("._file", 1), ("._missing", 9)):
            self.sidecar(name, kind)
            self.assertFalse(self.compare()["content_matches"])
            (self.root / name).unlink()
        (self.root / "._file").write_bytes(b"not AppleDouble")
        self.assertFalse(self.compare()["content_matches"])

    def test_content_corruption_still_fails_with_valid_sidecar(self):
        self.sidecar()
        (self.root / "file").write_bytes(b"corruption")
        self.assertEqual(self.compare()["changed"], ["file"])

    def test_truncated_and_overlapping_descriptors_fail(self):
        self.sidecar()
        p = self.root / "._file"
        original = p.read_bytes()
        p.write_bytes(original[:-1])
        self.assertFalse(self.compare()["content_matches"])
        p.write_bytes(original[:42] + struct.pack(">I", 51) + original[46:])
        self.assertFalse(self.compare()["content_matches"])

    def test_xattrs_are_separate_explicit_metadata(self):
        with mock.patch.object(metadata.os, "listxattr", return_value=["com.apple.test"], create=True), \
             mock.patch.object(metadata.os, "getxattr", return_value=b"metadata", create=True):
            result = self.compare()
        self.assertTrue(result["content_matches"])
        self.assertEqual(result["xattrs"]["file"]["com.apple.test"]["bytes"], 8)

    def test_failed_comparison_preserves_raw_evidence(self):
        (self.root / "file").write_bytes(b"corrupt")
        with self.assertRaises(RuntimeError):
            metadata.verify(self.root, self.expected, update.tree(self.root), self.root, "failure", {})
        self.assertTrue((self.root / "failure-tree.json").exists())


if __name__ == "__main__":
    unittest.main()
