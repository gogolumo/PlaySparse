# Roadmap

## M0 — Feasibility research
**Goal:** prove or reject the core storage assumptions before building a filesystem.

- document limits and threat model;
- benchmark random-access chunk compression;
- choose initial platform and implementation language;
- define reproducible metrics.

**Acceptance:** Experiment 01 produces machine-readable results and clearly states what it does *not* prove.

## M1 — Chunker
- fixed-size baseline;
- FastCDC/content-defined chunking experiment;
- deterministic chunk manifests;
- integrity checks.

**Acceptance:** arbitrary directory can be chunked and reconstructed byte-for-byte.

## M2 — Compressed object store
- BLAKE3-addressed objects;
- Zstd compression;
- atomic writes;
- corruption detection.

**Acceptance:** duplicate chunks are physically stored once and all reconstructed hashes match.

## M3 — Manifest format
- versioned schema;
- file metadata;
- chunk offsets/sizes;
- forward-compatible feature flags.

## M4 — Virtual filesystem
**Windows-first.** Compare WinFsp vs ProjFS for the first provider.

**Acceptance:** a read-only mounted/projected tree serves files from the compressed store without full extraction.

## M5 — Cache manager
- hot/warm/cold tiers;
- LRU/size-aware eviction;
- startup pinning;
- read telemetry.

## M6 — Directory reconstruction
- permissions/metadata handling;
- sparse files where valid;
- byte-identical validation suite.

## M7 — Deduplication
- FastCDC vs fixed chunks;
- cross-file and cross-install duplicate accounting;
- dedup benchmark corpus.

## M8 — Benchmark suite
- compression ratio;
- decompression throughput;
- random read p50/p95/p99;
- CPU/RAM;
- cold/warm launch impact;
- cache hit rate.

## M9 — Game experiments
Only on legally owned installations and without DRM/anti-cheat circumvention.

## M10 — Optional secondary/remote backing
- another local disk / NAS first;
- remote object store later;
- prefetch and offline behavior.
