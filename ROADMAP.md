# Roadmap

## Runtime sprint — real reads from compressed CAS

- [x] Rust workspace and in-process BLAKE3/Zstd/FastCDC
- [x] Immutable v1 packfiles, sorted binary index and verify-before-publish transaction
- [x] Experiment 04: loose/pack comparisons, native read baseline and real measurements
- [x] Binary-search byte ranges, 64-bit offsets, bounded LRU and single-flight
- [x] Correctness/property tests, malformed/corrupt object checks and optional fuzz targets
- [x] Genuine Linux FUSE reads, mmap, concurrent requests and executable launch
- [x] Generated 10 GiB corpus; mounted offsets beyond 4/8 GiB and EOF checks
- [x] Open-source native Zstd application executed from the mounted directory
- [x] Rust CLI: analyze/pack/verify/mount/unmount/benchmark/doctor and independent io-probe
- [x] Native WinFsp runtime validation in hosted Windows Server 2025 CI
- [ ] Physical Windows desktop/game validation
- [ ] Physical macFUSE validation (backend type-checked, experimental)
- [ ] Windows original vs WOF vs PlaySparse frontier and real open game compatibility
- [ ] Power-loss durability tests, installer/service and writable application state

Evidence: [`docs/evidence/first-mounted-run.md`](docs/evidence/first-mounted-run.md).
**WINDOWS HARDWARE TEST REQUIRED.** This sprint's Windows production milestone is
not declared complete. The historical research milestones below remain intact.
Tracing/adaptive policies follow validated Windows runtime behavior.

## M0 — Feasibility baseline ✅

- reality check and compression limits;
- Zstd random-access chunk experiment;
- Windows-first platform direction;
- measurement policy.

## M1 — Chunk identity and update reuse 🟡

### Experiment 02 — fixed vs CDC ✅

- synthetic insertion/replacement workload;
- fixed-offset baseline;
- FastCDC-style Gear reference;
- update chunk reuse, compressed-store size, CPU and manifest metrics.

**Result:** CDC strongly improves reuse on the synthetic insertion workload, but the Python implementation is intentionally non-production and CPU-heavy.

### Production follow-up

- replace research CDC with vetted Rust FastCDC implementation;
- repeat on L1 open game-like update corpora;
- test larger chunk-size families;
- reject CDC as a central feature if gains disappear outside insertion-heavy updates.

## M2 — Content-addressed compressed store 🟡

### Experiment 03 — BLAKE3 CAS ✅

- BLAKE3-256 object identity;
- Zstd immutable objects;
- manifest v0;
- atomic object publication;
- cross-file object reuse;
- per-object and per-file verification;
- byte-identical directory round trip.

### Production follow-up

- Rust storage engine;
- official BLAKE3 crate;
- in-process Zstd;
- multi-writer/journal design;
- crash/fault injection.

## M3 — Packfiles + indexes 🟡

- compare loose objects with packfiles;
- binary index;
- random object lookup latency;
- compaction/recovery;
- metadata overhead at millions of chunks.

## M4 — Byte-range resolver ✅

- serve `(path, offset, length)` directly from CAS;
- no whole-file reconstruction;
- concurrent reads;
- bounded request buffers and cached chunk reuse;
- read amplification metrics.

## M5 — Windows virtual filesystem 🟡

WinFsp's direct-buffer read contract is the implemented backend. ProjFS was
evaluated and rejected for this stage because it hydrates retrieved file data
into the local filesystem. Native Windows hardware tests and WOF baselines remain.

Required compatibility:

- directory enumeration;
- stat/open/close;
- sequential and random reads;
- concurrent reads;
- large offsets/files;
- memory-mapped reads;
- read-only launch workloads.

**Acceptance:** arbitrary readers see the same logical bytes without a full pre-extraction step.

## M6 — Trace profiler + static policy frontier

- record path/offset/length/timestamp/thread;
- establish fixed chunk/codec/cache baselines;
- measure p50/p95/p99, CPU, RAM, physical bytes and amplification.

## M7 — Adaptive chunk/codec policy

- file-aware policies;
- region-aware policies;
- hot/raw or LZ4 vs warm/cold Zstd;
- reject adaptive policy unless it beats static baselines on the same trace.

## M8 — Cache and prefetch

- LRU/LFU/2Q/ARC baselines;
- sequential predictor;
- transition/access graph;
- wasted-prefetch accounting;
- compressed vs decompressed cache state.

## M9 — Tiered storage

- primary SSD + secondary disk first;
- NAS second;
- remote object backing only after local behavior is robust;
- hydration/offline/failure semantics.

## M10 — Representative workloads

Evidence progression:

- L0 synthetic;
- L1 redistributable/open game-like corpus;
- L2 local legally-owned game installs (results only, no assets);
- L3 replicated cross-game/hardware results.

## M11 — Productization

Only after the engine is correct and benchmarks justify it:

- stable CLI (`analyze`, `pack`, `verify`, `mount`, `unmount`, `profile`, `optimize`, `doctor`);
- installer/service lifecycle;
- recovery tooling;
- GUI.
