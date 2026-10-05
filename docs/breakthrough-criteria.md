# Breakthrough criteria

PlaySparse does not earn the word "breakthrough" because an experiment works. It must beat meaningful baselines on reproducible workloads.

## Baselines

At minimum compare against:

1. uncompressed source files;
2. Zstd fixed-chunk store;
3. native filesystem compression on the target OS where available (WOF/CompactOS on Windows, btrfs compression on Linux test systems);
4. static chunk/codec/cache configurations from PlaySparse itself.

## Primary frontier

Every configuration is a point in a multi-objective space:

- physical bytes on primary storage;
- p50/p95/p99 random-read latency;
- sequential throughput;
- CPU time / CPU utilization;
- RAM/cache bytes;
- read amplification;
- write amplification;
- cold and warm startup time.

A configuration that is worse than another on both storage and latency without compensating value is **dominated** and should be rejected.

## Research success conditions

A result becomes interesting when it produces a material, reproducible non-dominated improvement such as one of the following:

- at comparable p95 latency, materially lower physical storage than the best static baseline;
- at comparable physical storage, materially lower p95/p99 latency;
- after a realistic update, materially fewer new physical bytes due to stable content-defined chunk reuse;
- with secondary storage, substantially lower primary-SSD residency while keeping measured gameplay/startup stalls within an explicit budget;
- a trace-adaptive policy beats every static policy on the same workload across repeated runs.

The earlier `>=20%` idea is treated only as an aspirational screening threshold, not a definition of novelty.

## Kill criteria

Reject or demote a technique when the measured gain is trivial relative to cost. Examples:

- <1% space improvement with large CPU/latency overhead;
- deduplication below measurement noise on representative corpora;
- prefetch that increases I/O materially without improving p95/p99 latency;
- adaptive policy that fails to beat a simpler static policy;
- CDC gains that appear only on synthetic insertion tests but disappear on real update corpora.

Negative results remain in the repository.

## Evidence levels

- **L0 — synthetic:** generated data; validates mechanics only.
- **L1 — open game-like:** redistributable/open assets and archives.
- **L2 — local real game:** locally measured legally owned install; no copyrighted data committed.
- **L3 — cross-game replication:** same conclusion reproduced across several unrelated games/hardware profiles.

No industry claim should be based on L0 alone.


## Hardware and compatibility gates

Evidence level and host type are separate. Linux VM FUSE success, hosted native
WinFsp success, and macFUSE SDK compile/link success do not establish physical
Linux/Windows desktop or native Mac mounted runtime evidence. Driver presence
also does not establish kernel approval: the actual doctor mount/read/unmount
probe must pass before the POSIX runtime stages.

A direct owned executable launch records that application's result. Generated
TestGame is L0 even if classified as a game by a caller. Launcher compatibility
stays `NOT RUN` until that launcher itself has been validated. A passed single
application never establishes general game compatibility.

The [Windows comparison](windows-comparison.md) measures identical readonly
requests across original files, a verified disposable WOF copy, and PlaySparse.
It reports warm trials, allocated file streams, CPU/RSS and measurable latency;
startup is `NOT RUN` and native/WOF physical read amplification is null when
kernel/device bytes cannot be measured. No absent measurement becomes zero.
Physical WOF evidence requires Windows client hardware attestation and successful
software/integrity checks; hosted results retain a blocked physical gate.

The [readiness policy replay](evidence/production-readiness-policy.md) reduces
speculative bytes but still fails the static latency/CPU baseline. It does not
qualify adaptive policy as preferred. Repeated wins on identical traces across
representative workloads are required before changing the default.
