# Benchmark policy

Every published result must include:

- dataset name and construction/source;
- original byte size;
- stored byte size;
- compression level;
- chunk size;
- compression wall time;
- sequential decompression throughput where measured;
- random-read p50/p95/p99 latency;
- OS/architecture;
- Zstd version;
- explicit statement whether the dataset is synthetic or a real installation.

Synthetic data is useful for testing mechanics, but it must never be presented as evidence that a particular game will achieve the same ratio.
