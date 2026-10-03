# Runtime sprint audit

Starting main: `dffb875` (`feat: build PlaySparse CAS research foundation`).
The checkout was fetched, switched to main and updated with `pull --ff-only`
before changes. Development host: macOS arm64; Windows hardware is unavailable.

The existing README, ROADMAP, docs, lab source, experiments 01–03, raw results,
tests and research CI were inspected. None of these experiments are deleted or
relabelled as mounted filesystem evidence.

The v0 Python CAS uses full-file `read_bytes`, a reference BLAKE3 implementation,
one Zstd subprocess per object, loose objects, and full-file reconstruction.
Object identity and full reconstruction correctness were already established.
The suite lacks a mounted backend, range reader, cache, executable/mmap probe,
64-bit large-file tests and production transaction-level store publication.
Experiment 01 reports process-launch overhead explicitly; its measured latencies
are not Rust runtime predictions. Experiment 02 establishes synthetic update
reuse, not real-game savings. Experiment 03 proves reconstruction, not VFS reads.

The new workspace has core/store/cache/range/CLI crates and separate platform
backends. In-process hashing and compression replace the Python production path;
the reference implementation remains usable. Packfiles and binary indexes are
implemented alongside loose objects for Experiment 04. Backend code consumes
the same resolver, so normal filesystem reads and direct tests share semantics.

Dependency versions were checked against crates.io and official crate docs on
2026-10-02, then frozen in Cargo.lock. Current candidates included
[blake3 1.8.7](https://docs.rs/blake3/1.8.7/blake3/),
[zstd 0.14.0](https://docs.rs/zstd/0.14.0/zstd/),
[fastcdc 5.0.0](https://docs.rs/fastcdc/5.0.0/fastcdc/v2020/),
[fuser 0.18.0](https://docs.rs/fuser/0.18.0/fuser/) and
[memmap2 0.9.11](https://docs.rs/memmap2/0.9.11/memmap2/).
The crates.io API additionally checked serde, serde_json, clap, anyhow, thiserror,
LRU, parking_lot, tempfile, proptest, tracing, walkdir, libc and ctrlc. Stable libc
0.2 is retained instead of the newer 1.0 alpha. Tokio is not required.

New evidence must distinguish: host tests, Linux VM real FUSE tests, compile-only
Windows checks, Windows hardware execution and native WOF baseline measurements.
Only recorded commands and real output can establish a working capability.


## Adaptive writable sprint audit (2026-10-03)

Starting main for this sprint: `09e90e635c6b5d2423243e02bd7df3f7dd94e638`.
A dedicated `feat/adaptive-writable-runtime` branch preserves the preceding
runtime and its historical evidence. At start, main's Linux and hosted Windows
checks passed; PR #1 was merged, with no open issues or PRs. During the sprint,
PR #2 published the overlay/trace checkpoint and was subsequently merged by an
external action. Its Windows fixes were fetched and incorporated before adding
policy/tiers; no main history was overwritten.

The audited foundation already had immutable CAS, verified packfiles/loose
objects, byte ranges, bounded single-flight LRU, real FUSE and WinFsp read-only
mounts, mapped reads, native executables and generated 10 GiB offsets. Missing
pieces were persistent mutable namespace, write callback adapters, access
telemetry, policy behavior and independent physical object sources. Shared
Overlay, Trace and Policy crates reuse the existing resolver and Store.

The development Mac can execute portable Rust tests and host builds. Its
Linux VM exposes real `/dev/fuse`, enabling ordinary mounted updater, mmap,
executable, adaptive and HTTP-tier tests. macFUSE is absent; installation needs
explicit user approval outside this task. Hosted Windows Server runners provide
native MSVC/WinFsp execution, while physical Windows desktop and user-owned game
validation remain unavailable. An optional PowerShell harness records these
separate gates rather than converting hosted results into hardware claims.

Observed failures remain evidence: Windows verbatim-prefix startup, hardcoded
read-only file modes, small unbuffered fixture reads, Linux unlinked-handle
metadata and a dispatcher-bound sequential detector. Regression tests and real
reruns verify fixes. Initial zero-prefetch/negative policy measurements are kept;
no historical experiment or failed result was replaced by a favorable number.
