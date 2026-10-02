# Experiment 02 results

> Synthetic patch-resilience benchmark; this is not a commercial-game result.

Base: **24.00 MiB**; updated: **24.12 MiB**.
Update: 128 KiB insertion plus 96 KiB replacement.

| Chunker | v2 bytes reused from v1 | Unique raw / two versions | Compressed unique / two versions | Chunking v1+v2 | Manifest |
|---|---:|---:|---:|---:|---:|
| fixed | 33.16% | 83.38% | 16.86% | 0.007s | 18.2 KiB |
| fastcdc-style | 92.91% | 53.42% | 10.89% | 6.549s | 6.9 KiB |

## Interpretation

CDC changed update reuse by **+59.75 percentage points** versus fixed offsets.
Its compressed two-version store changed by **-35.39%** versus fixed chunks.

This experiment tests one thing: whether content-defined boundaries recover after inserted data and therefore improve version reuse. It does not establish that CDC saves meaningful space inside a single already-compressed AAA install.

The implementation is a small FastCDC-style normalized Gear reference. Production code should use a vetted Rust FastCDC implementation and repeat this benchmark on open game-like corpora.
