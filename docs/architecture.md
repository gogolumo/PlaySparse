# Architecture

## Proposed data path

```text
read(path, offset, length)
        |
        v
Manifest lookup
        |
        v
Chunk span calculation
        |
        +---- cache hit ----> bytes
        |
        v
Object store lookup
        |
        v
Zstd decompress independent chunk
        |
        v
verify hash (policy dependent)
        |
        v
cache + slice requested range
        |
        v
return bytes
```

## Storage model

Future manifests should map logical file ranges to content-addressed chunks:

```json
{
  "path": "assets/world.pak",
  "size": 829381293,
  "chunks": [
    {
      "hash": "blake3:...",
      "file_offset": 0,
      "raw_size": 4194304,
      "stored_size": 1738291
    }
  ]
}
```

Objects are immutable and addressed by content hash. The same chunk referenced by several files or installations is stored once.

## Why independent frames/chunks

Monolithic compression usually maximizes cross-range history but creates poor random-read behavior. Independent chunks bound the amount of data that must be decompressed for a small random read. Experiment 01 measures that first-order trade-off.

## Filesystem strategy

### Windows first
Two serious prototypes should be compared:

- **WinFsp:** flexible user-mode filesystem with broad filesystem semantics.
- **ProjFS:** Windows projection API designed to make backing-store data appear locally present; attractive for a read-mostly projected tree.

The first production-like experiment should remain read-only. Writes, patchers and launchers substantially complicate correctness.

### macOS
macFUSE is viable for experimentation. On current macOS generations, macFUSE also has an FSKit backend, reducing dependence on legacy kernel-extension workflows.

### Linux
FUSE is the straightforward experimental target.

## Integrity

Safe mode must be capable of reconstructing every original file byte-for-byte. Manifests need versioning and object hashes. Source files remain untouched until the project has proven reconstruction correctness over large corpora.
