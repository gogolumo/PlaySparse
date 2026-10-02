# Experiment 01 — Zstd random-access chunk trade-off

## Question

Can independent Zstd-compressed chunks provide useful compression while bounding the amount of data decompressed for small random reads?

## Hypothesis

Smaller chunks should reduce random-read amplification and latency, but may worsen compression ratio and incur more metadata/process overhead. Larger chunks should improve ratio but increase decompression work per random read.

## Method

The script generates a synthetic mixed-entropy binary dataset containing:

- highly compressible repeated text/pattern data;
- zero-filled data;
- pseudo-random high-entropy data;
- repeated structured binary blocks.

It then measures:

1. monolithic Zstd level-3 compression;
2. independent chunk compression at 256 KiB, 1 MiB, 4 MiB and 16 MiB;
3. sequential reconstruction throughput for each chunk layout;
4. p50/p95/p99 latency of random 4 KiB reads, where the containing chunk is decompressed on demand.

## Important limitation

The experiment invokes the `zstd` CLI once per chunk/read. That adds process-spawn overhead that a real Rust/libzstd implementation would not have. Therefore absolute random-read latency is **not** a production prediction. The experiment is intended to compare chunk-size directionally and validate the data format/measurement pipeline.

## Run

```bash
python3 benchmark.py
```

Optional:

```bash
python3 benchmark.py --dataset-mib 96 --random-reads 200 --level 3
```

Outputs:

- `results/latest.json`
- `results/latest.md`

Generated dataset and compressed work files are ignored by Git.
