# Rust storage format v1

Experimental immutable read-only format. Python v0 remains the research reference;
v0 stores are not silently reinterpreted as v1. Repack from the original directory.

```
store/
  COMMITTED.json
  manifest.json
  index/objects.idx
  packs/pack-0000.psp
```

The benchmark-only loose layout replaces packs with `objects/ab/<62 hex>.pso`.
Object identity is always BLAKE3-256 of raw bytes. Each object independently uses
raw or Zstd; raw wins when compression does not reduce its encoded byte length.
There are no dictionaries, previous-frame dependencies or hydrated source files.

The deterministic UTF-8 JSON manifest contains sorted file and directory paths,
file sizes (`u64`), permission bits, whole-file BLAKE3, and contiguous chunk tables
with `(hash, logical offset u64, raw size u32)`. Empty directories/files survive.
Symlinks, special files, non-UTF-8 names, path traversal, colon/backslash names and
control characters are rejected. Timestamps, hard-link identity, ACLs and Windows
alternate data streams are not preserved in this version. Windows case collisions
must be rejected by the backend rather than ambiguously mounted.

`objects.idx` is little-endian: 8 bytes `PSPIDX01`, object count `u64`, then sorted
56-byte records. There is exactly one record per raw digest:

| Field | Bytes |
|---|---:|
| raw BLAKE3 | 32 |
| pack ID | 4 |
| byte offset in pack | 8 |
| compressed size | 4 |
| raw size | 4 |
| codec (0 raw, 1 Zstd) | 1 |
| reserved, must be zero | 3 |

Packs start with `PSPPACK1`, followed by object payloads without padding. They
roll at 256 MiB. Opening validates the version, metadata digests, strict sorting,
chunk coverage, object bounds, pack headers and exact physical pack coverage.
Every loaded object is decompressed to an explicit size bound and verified with
BLAKE3 before being returned or admitted to the cache. Full `verify` additionally
checks whole-file digests incrementally and validates unreferenced objects.

Metadata files are limited to 256 MiB each; chunks to 16 MiB. The CLI validates
power-of-two target chunks between 4 KiB and 4 MiB before calling FastCDC. CDC is
the vetted `fastcdc::v2020::StreamCDC`, using min=target/4 and max=target*4, with
bounded streaming input. Fixed chunks remain a measured baseline.

## Range and cache contract

`RangeResolver::read_range(path, offset: u64, length: usize)` performs binary
searches over the sorted file table and chunk offsets, then loads only objects
intersecting the clamped range. EOF and zero-length requests return empty bytes.
Individual requests above 16 MiB return a typed error; filesystem callbacks and
streaming callers must issue bounded reads. No whole-file reconstruction API is
used in the production read path.

The byte-weighted LRU stores verified decompressed chunks. Cache capacity includes
resident cached raw bytes; metadata, request buffers, active caller references,
compressed buffers and codec workspaces are additional process memory. At most
eight independent loads run at once. A per-digest condition variable shares one
load with concurrent callers, including callers with a zero-capacity cache.
Metrics expose hits, misses, evictions, resident bytes, loads, avoided loads,
in-flight waits and loaded raw bytes. `decompressions` counts object loads,
including raw-codec loads; inspect codec records for literal Zstd invocations.

## Transaction and recovery

Source files are opened read-only; their lengths and modification times are
checked before/after streaming. Pack goes to a sibling unique temporary directory
on the destination filesystem, fsyncs objects and metadata, verifies the complete
store, writes/fsyncs a metadata-digest commit marker, fsyncs directories on Unix,
then atomically renames the directory and fsyncs its parent. Existing destinations
are refused. Output inside the source tree is refused. Originals are never removed.

A destination-specific OS advisory file lock prevents concurrent writers to the
same destination and is released by the OS after a killed process. The small
sibling lock file remains to keep its inode stable and is reused on retry. A
killed process may leave an uncommitted staging directory; after confirming no
pack process is active an operator may remove that specific staging path. It is
never automatically promoted. A staging tree without `COMMITTED.json` is rejected.
Linux and macOS publication uses exclusive rename, refusing even an externally
created empty destination directory during the publication race.
Windows directory durability and real power-loss behavior require hardware tests;
Rust's Unix directory-fsync behavior is not claimed for Windows. There is no
multi-writer mutation, compaction, garbage collection or transactional update yet.

The integrity seal detects accidental metadata corruption, not malicious
replacement by an attacker able to rewrite all digests. Stores must remain
immutable while readers run; no facility replaces an open store in place.
