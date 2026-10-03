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
- [ ] **HARDWARE REQUIRED:** physical Windows desktop/game validation
- [ ] **HARDWARE REQUIRED:** physical macFUSE validation (backend type-checked, experimental)
- [ ] Windows original vs WOF vs PlaySparse frontier and real open game compatibility
- [ ] Power-loss durability tests and installer/service lifecycle

Evidence: [`docs/evidence/first-mounted-run.md`](docs/evidence/first-mounted-run.md).
**WINDOWS HARDWARE TEST REQUIRED.** This sprint's Windows production milestone is
not declared complete. The historical research milestones below remain intact.

## Adaptive writable runtime sprint — EXPERIMENTAL

- [x] Persistent merged namespace over an immutable base, tombstones and whole-file copy-up
- [x] Create/write/append/truncate/delete/rename, stable handles and remount persistence
- [x] Overlay status/discard and commit into a new verified store
- [x] Generated updater through an actual Linux FUSE mount; source/base byte invariance
- [x] Native Windows writable baseline in hosted CI at `668d70f` (PR #2)
- [x] Bounded JSONL callback trace, operation/source/error records and summaries
- [x] Strict versioned policy JSON, observed file heat and decaying retention priorities
- [x] Bounded sequential prefetch with byte/queue/TTL limits and useful/wasted accounting
- [x] Static/adaptive replay of the same traced reads with byte verification and resource metrics
- [x] Optional secondary local objects and separate verified promotion cache
- [x] Exact HTTP 206 object ranges, verification, bounded failure/retry and offline local reads
- [x] Actual Linux mounted overlay/trace/tier integration checks
- [x] Native Windows combined writable/adaptive/tiered runtime at `4e39b9c` (hosted Server 2025)
- [ ] Reproducible non-dominated adaptive benefit on representative workloads
- [ ] **HARDWARE REQUIRED:** physical Windows desktop and legally owned game/launcher checks
- [ ] **HARDWARE REQUIRED:** physical macFUSE checks
- [ ] Block/chunk copy-up, mutable-data compression, advanced timestamps/ACL semantics and power-loss proof

Implementation is ahead of compatibility and performance proof. The original
Linux adaptive benchmark produced zero prefetch requests and a lower adaptive
hit ratio; that negative result is retained. The corrected sequential detector
produces real prefetch, but the final exact-policy replay still loses on latency,
CPU and loaded bytes. Both results and all trials remain in the evidence.
A generated updater does not establish Steam/Epic or
anti-cheat compatibility. Overlay commit currently requires a temporary full
logical merged tree before packing its new store.

See [sprint evidence](docs/evidence/adaptive-writable-runtime.md),
[overlay semantics](docs/writable-overlay.md),
[adaptive policy](docs/adaptive-policy.md) and
[local/HTTP tiers](docs/tiered-storage.md).

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

- [x] Replace research CDC with the Rust FastCDC implementation
- [ ] Repeat on L1 open game-like update corpora
- [ ] Test larger chunk-size families
- [ ] Reject CDC as a central feature if gains disappear outside insertion-heavy updates

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

- [x] Rust immutable storage engine, official BLAKE3 crate and in-process Zstd
- [x] Verify-before-publish transaction and generated process-interruption/corruption checks
- [ ] Multi-writer/journal design
- [ ] Broader fault injection and physical power-loss validation

## M3 — Packfiles + indexes 🟡

- [x] Compare loose objects with packfiles in Experiment 04
- [x] Sorted binary index and measured random object lookup latency
- [ ] Compaction/recovery
- [ ] Metadata overhead at millions of chunks

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

Hosted native CI has proved those read workloads and the combined writable,
adaptive and tiered paths at `4e39b9c`. Physical client Windows and real
applications remain separate validation gates.

## M6 — Trace profiler + static policy frontier 🟡

- [x] Record versioned path/offset/length/time/worker/operation/cache/source/error events
- [x] Bounded trace writer, dropped-event/error metrics and validated summaries
- [x] Static/adaptive identical-trace replay with p50/p95/p99, CPU/RSS and amplification
- [ ] Complete representative chunk/codec/cache frontier including physical device allocation

## M7 — Adaptive chunk/codec policy 🟡

- [x] File priorities and decaying observed hotness for cache retention
- [x] Persisted, strictly validated policy from a recorded trace
- [ ] Adaptive region/chunk-size/codec selection
- [ ] Hot/raw or LZ4 vs warm/cold Zstd
- [ ] Demonstrate a non-dominated improvement over static baselines on identical traces

## M8 — Cache and prefetch 🟡

- [x] Byte-bounded static LRU and decaying-priority eviction
- [x] Bounded per-file sequential predictor, including advancing overlapping kernel reads
- [x] Single prefetch worker, bounded reservations/queue/TTL and useful/wasted/error accounting
- [ ] LFU/2Q/ARC comparison baselines
- [ ] Transition/access graph
- [ ] Compressed vs decompressed cache state
- [ ] Representative mounted performance acceptance, including adverse workloads

## M9 — Tiered storage 🟡

- [x] Primary local and optional secondary local verified objects
- [x] Separate raw promotion cache and local reads after the remote disappears
- [x] Sealed local metadata with exact HTTP object ranges and integrity verification
- [x] Bounded timeout/retry and explicit protocol/corruption failures; actual Linux mounted checks
- [ ] Physical SSD/HDD/NAS measurements and representative network conditions
- [ ] Promotion-cache disk eviction/capacity policy
- [x] Native Windows combined tier validation in hosted Server 2025 CI
- [ ] Representative application compatibility

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
