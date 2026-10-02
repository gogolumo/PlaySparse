# Experiment 01 results

- Dataset: **synthetic mixed-entropy baseline (25% repetitive text, 25% zeros, 25% deterministic high-entropy-like bytes, 25% structured repeated binary)**
- Original size: **32.00 MiB**
- Zstd: `*** Zstandard CLI (64-bit) v1.5.7, by Yann Collet ***`
- Platform: `Linux-6.18.44-x86_64-with-glibc2.41`
- Compression level: **3**
- Random read: **4096 bytes**, 50 samples

## Results

| Layout | Stored MiB | Ratio (orig/stored) | Compress s | Reconstruct MiB/s | Random p50 ms | p95 ms | p99 ms |
|---|---:|---:|---:|---:|---:|---:|---:|
| monolithic | 8.00 | 4.00x | 0.021 | n/a | n/a | n/a | n/a |
| 256 KiB | 8.01 | 3.99x | 0.204 | 187.2 | 1.354 | 1.675 | 1.866 |
| 1 MiB | 8.00 | 4.00x | 0.088 | 553.2 | 1.938 | 3.755 | 5.155 |
| 4 MiB | 8.00 | 4.00x | 0.045 | 1107.4 | 4.849 | 7.170 | 7.550 |
| 16 MiB | 8.00 | 4.00x | 0.026 | 2397.8 | 15.413 | 18.727 | 19.172 |

## Interpretation

This synthetic dataset deliberately mixes compressible and high-entropy regions. It validates the benchmark harness and the chunk-size trade-off; it does **not** predict the compression ratio of GTA, Dota, or any other game.

The random-read numbers include a full `zstd` process launch per read, so they overstate the latency of a future in-process libzstd implementation. Compare chunk sizes directionally rather than treating the absolute values as a product target.

Safe-mode integrity check: reconstructed SHA-256 must equal the original SHA-256 for every chunk layout.

Original SHA-256: `09f2e29b63052b97201d8c6f04e44886ac4eb6a4e9eb985d3365fdbbd322ea74`
