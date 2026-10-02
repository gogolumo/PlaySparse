# PlaySparse storage format v0

Status: **experimental / not stable**.

Format v0 exists to make Experiment 03 concrete. It is intentionally simple enough to verify before packfiles, VFS callbacks and adaptive policies are introduced.

## Directory layout

```text
store/
├── manifest.json
└── objects/
    ├── ab/
    │   └── cdef....zst
    └── 42/
        └── 91aa....zst
```

Objects are immutable compressed chunks. The logical object ID is BLAKE3-256 of the **uncompressed** chunk bytes.

## Object path

For digest:

```text
abcdef0123...
```

v0 stores:

```text
objects/ab/cdef0123....zst
```

The directory split avoids placing all objects in one directory.

## Manifest

```json
{
  "format": "playsparse-store",
  "version": 0,
  "hash": "blake3-256",
  "codec": { "name": "zstd", "level": 3 },
  "chunker": {
    "algorithm": "fastcdc-style-gear-v0",
    "min": 65536,
    "avg": 262144,
    "max": 1048576
  },
  "files": [
    {
      "path": "assets/world.pak",
      "size": 123456,
      "sha256": "...",
      "mode": 420,
      "chunks": [
        {
          "hash": "blake3:...",
          "offset": 0,
          "raw_size": 262144,
          "stored_size": 90123,
          "codec": "zstd"
        }
      ]
    }
  ]
}
```

## Read contract

To service `(path, offset, length)`:

1. locate the file entry;
2. locate chunks intersecting the requested range;
3. load the immutable objects;
4. decompress and validate BLAKE3;
5. slice only the requested logical range;
6. optionally place raw/compressed representations in cache.

A future VFS should use this contract without reconstructing the entire file first.

## Integrity

- object identity: BLAKE3-256 of raw chunk bytes;
- file verification: SHA-256 in v0 experiments for an independent end-to-end check;
- missing object: hard error;
- hash mismatch: hard corruption error.

## Crash safety

New objects are written to a temporary file in the destination directory, flushed, and atomically renamed into place. The manifest is published only after object writes complete.

v0 does not yet include a journal for multi-process writers. It should be treated as single-writer.

## Why loose objects first

Loose objects make correctness inspectable. They are *not* expected to be the final layout because millions of chunks would create metadata and directory overhead. Experiment 04 will compare loose objects against packfiles plus indexes.

## Planned incompatible changes

- canonical FastCDC implementation in Rust;
- packfile object storage;
- binary/versioned manifest;
- per-object codec field enabling raw/LZ4/Zstd selection;
- storage-tier locator;
- access-temperature/profile metadata stored separately from immutable content identity.
