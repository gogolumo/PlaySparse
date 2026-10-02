import tempfile
import unittest
from pathlib import Path
import shutil

from playsparse_lab.blake3_ref import hexdigest
from playsparse_lab.chunking import fixed_chunks, fastcdc_chunks
from playsparse_lab.cas import pack_directory, verify_store, unpack_store, tree_sha256


class Blake3Tests(unittest.TestCase):
    def test_official_empty_vector(self):
        self.assertEqual(
            hexdigest(b""),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        )

    def test_official_abc_vector(self):
        self.assertEqual(
            hexdigest(b"abc"),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85",
        )


class ChunkingTests(unittest.TestCase):
    def test_roundtrip_boundaries(self):
        data = (b"abcdefgh" * 200_000) + bytes(range(256)) * 1000
        for chunks in (
            list(fixed_chunks(data, 256 * 1024)),
            list(fastcdc_chunks(data)),
        ):
            self.assertEqual(b"".join(c.data for c in chunks), data)
            self.assertEqual(chunks[0].offset, 0)

    def test_fastcdc_is_deterministic(self):
        data = bytes((i * 17 + i // 13) % 256 for i in range(2_000_000))
        a = [(c.offset, c.size) for c in fastcdc_chunks(data)]
        b = [(c.offset, c.size) for c in fastcdc_chunks(data)]
        self.assertEqual(a, b)


@unittest.skipUnless(shutil.which("zstd"), "zstd CLI required")
class CASTests(unittest.TestCase):
    def test_directory_roundtrip_and_dedup(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            src = root / "src"
            store = root / "store"
            out = root / "out"
            src.mkdir()
            block = (b"PLAYSPARSE" * 32768) + bytes(range(256)) * 1024
            (src / "a.bin").write_bytes(block)
            (src / "b.bin").write_bytes(block)
            (src / "small.txt").write_text("hello PlaySparse\n" * 100)
            stats = pack_directory(src, store)
            self.assertGreater(stats["reused_chunks"], 0)
            self.assertTrue(verify_store(store)["ok"])
            unpack_store(store, out)
            self.assertEqual(tree_sha256(src), tree_sha256(out))


if __name__ == "__main__":
    unittest.main()
