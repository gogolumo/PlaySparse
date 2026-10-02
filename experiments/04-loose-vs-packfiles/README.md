# Experiment 04 — Rust loose objects versus indexed packfiles

This experiment measures the same source bytes with two layouts in the Rust runtime. The default corpus is a deterministic 64 MiB mixed-entropy file, 512 distinct small assets and an exact duplicate. It deliberately tests filesystem object overhead; it is not a game dataset or a game compression estimate.

Build and run from the repository root:

```sh
cargo build --release --workspace
python3 experiments/04-loose-vs-packfiles/run_benchmark.py \
  --output /tmp/playsparse-exp04-results \
  --work /tmp/playsparse-exp04-work \
  --repetitions 3 --iterations 200
```

Use `--source /path/to/input` for an existing read-only corpus. The script hashes the complete source tree before and after, refuses existing output/work directories, alternates layout order and preserves every raw command output. `--chunker cdc` is available; the default fixed 256 KiB chunks isolate the layout comparison.

`result.json` contains pack time, object lookup p50/p95/p99, random object read p50/p95/p99, sequential throughput, encoded file lengths, allocated file blocks, directory block allocation, entry counts and directory enumeration timings. BLAKE3 verification and every benchmark byte comparison must succeed. The production default is packfiles, with loose storage retained as a measured baseline.

The direct range benchmark calls the Rust resolver, rather than a mounted path. Its cold measurement disables the decompressed chunk cache; OS caches are uncontrolled. Mounted API overhead, executable loading and page faults are measured separately with `tools/mounted-smoke.py`. `st_blocks` allocation is available on POSIX; encoded lengths are portable. Results never infer disk-cold latency from a chunk-cache-cold label.

Measured results for this sprint are preserved under [`docs/evidence`](../../docs/evidence). Report slower cases alongside improvements: fewer files and faster pack publication do not guarantee lower random-read latency.
