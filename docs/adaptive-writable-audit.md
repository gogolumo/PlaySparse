# Adaptive writable runtime: starting audit

Starting HEAD: `09e90e635c6b5d2423243e02bd7df3f7dd94e638`.
Branch: `feat/adaptive-writable-runtime`. Audit date: 2026-10-03.

The starting workspace has validated immutable manifests, indexed raw/Zstd
objects, streaming pack/verify transactions, binary-search range resolution,
byte-bounded LRU and per-object single-flight. FUSE and WinFsp serve those ranges
through actual read-only mounts. Existing evidence includes 10 GiB logical files,
64-bit offsets, mmap, executable launch, concurrent I/O, corruption, SIGKILL,
ENOSPC and negative performance results. These paths and evidence are retained.

Inspected README, ROADMAP, workspace manifests/lock, architecture v1/v2, runtime
audit, limitations, both backend docs, storage format, breakthrough criteria and
prior art; core/store/cache/range/CLI/backend sources; io-probe, corpus generator,
mounted smoke/frontier runners and both workflows. GitHub had no open issues or
PRs; PR #1 was merged. The latest main rust-runtime and research-ci runs were
successful (37093992366 and 37093992502).

Missing runtime components at the starting commit: writable namespace/persistent
copy-up, synthetic mounted updater, tracing, adaptive policy and object tiers.
The reusable range resolver and verified object loader remain the immutable read
path. Backend callbacks should translate writes into one shared overlay engine;
they should not each implement a second storage model. Existing immutable inode
maps need a writable alternative with stable open handles across rename/unlink.

Constraints: FUSE uses kernel page caching for mmap/exec and user-only mounting;
WinFsp uses ordinal case-insensitive names, concrete file security rights, signed
file-size limits and driver-created mountpoints. Both need real callback tests.
Whole-file streaming copy-up is acceptable v1 with measured amplification;
block copy-on-write is a later optimization. Namespace updates must publish
atomically; interrupted in-place user data writes have ordinary filesystem
semantics and require explicit flush for durability.

Available validation: macOS arm64 workspace tests; privileged Linux Docker VM
with genuine /dev/fuse; native hosted Windows Server CI with pinned WinFsp 2.1.
The Mac has no macFUSE installation (pkgutil listing and driver path checked).
No driver or privileged extension is installed by this sprint. Physical Windows
hardware and user-owned game data are unavailable; hosted CI proves neither
physical desktop nor real game compatibility.
