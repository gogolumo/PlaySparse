# Experiment 02 — Fixed chunks vs FastCDC-style CDC

## Question

Does content-defined chunking materially improve reuse after a game-like update that inserts bytes and modifies a small region?

## Why it matters

Fixed offsets are fragile: one insertion can shift every later chunk boundary. CDC chooses boundaries from content, so unchanged regions can converge back to the same chunk identities. That is established prior art; this experiment determines whether it is useful enough for PlaySparse's version-aware store.

## Metrics

- bytes in v2 reusing a chunk already present in v1;
- unique raw bytes needed to keep both versions;
- compressed unique bytes after Zstd;
- chunking wall time;
- manifest size.

## Run

```bash
PYTHONPATH=../.. python3 benchmark.py
```

The dataset is synthetic and generated in memory. No copyrighted game data is committed.

## Important implementation note

The lab implementation follows FastCDC's published design principles (Gear hash, minimum-size skipping and normalized masks) but is not claimed byte-identical with a particular library. Production Rust code should use a vetted FastCDC implementation and preserve this benchmark as the behavioral acceptance test.
