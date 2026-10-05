# Experiment 05: game-aware packing

L0 generated fixtures only. Compare generic CDC and fixed against measured-codec
CDC and original ZIP-record CDC on the **same** v1/v2 bytes and requests.
All configurations use 256 KiB target, Zstd level 3 with raw fallback unless
explicitly testing measured raw, 16 MiB userspace cache and identical query seed.

```sh
cargo build --locked --release --workspace --all-features
python3 experiments/05-game-aware-packing/run_benchmark.py \
  --work /tmp/playsparse-game-aware-01 --size-mib 16 --repetitions 3 --iterations 200
python3 experiments/05-game-aware-packing/run_benchmark.py \
  --work /tmp/playsparse-game-aware-adverse-01 --size-mib 16 --repetitions 3 \
  --iterations 200 --adversarial
```

Each work path must be new, outside the repository. Optional `--baseline PATH`
and `--baseline-commit SHA` compare a preserved current-main binary; otherwise
the same binary's unchanged generic path is used. The old reader verifies every
candidate store. Engine labels do not choose a codec; no UM/game/network is
required. Fixture/archive generation occurs only in the new scratch directory.

The primary fixture contains deterministic deflated random ZIP entries, an
incompressible file and compressible script data. V2 inserts one record and
replaces 8 KiB of another. The adverse fixture adds a mostly compressible file
whose three exact probe regions are random, exposing unsafe generalization
from bounded samples. This deliberately negative case is retained.

Modes rotate across repetitions. Evidence includes all command logs, profiles,
plans, source inventories/hashes, binary/repository provenance, medians and
variance. V2 measurements retain object/metadata/physical/allocated bytes, wall
and process CPU including plan validation, separate inspect+plan cost, unique
objects/reuse, cold/warm p50/p95/p99 for 4/64/1024 KiB queries, sequential throughput,
read benchmark CPU/RSS/cache/amplification and byte-identity checks. Summary
latencies use 4 KiB; full request-size distributions remain in each raw run.

`update_new_physical_bytes` is the measured missing encoded object payload sum
plus complete v2 metadata and one projected pack header. It describes a potential
shared-CAS byte budget, not actual allocated bytes of a published delta store.
Independently published v2 total bytes are reported separately. OS/device caches
are uncontrolled; “cold” disables the decompressed userspace cache. Read CPU
includes original comparison reads, hashing, store opens and all benchmark phases.
Absent counters stay null. Startup, mounted performance, native compression,
L1 open workloads and L2 owned games are NOT RUN.

The first measurements included ZIP+measured-raw together. After the adverse
sampling regression, the final matrix retains Zstd for ZIP mode and makes raw
selection a separate opt-in. Both earlier runs and all final runs remain in the
[evidence report](../../docs/evidence/game-awareness.md); no fastest trial is chosen.

No fixture/store/asset payload is committed, only code and measured JSON/logs.
