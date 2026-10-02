# PlaySparse

**Experimental transparent storage for very large games.**

Modern games are huge: 100 GB, 150 GB, sometimes more than 200 GB. PlaySparse explores whether a game can occupy substantially less **local physical storage** while still presenting a normal file tree to the game at runtime.

The project investigates a storage layer combining:

- transparent chunk compression;
- content-addressable storage and deduplication;
- random-access decompression;
- a virtual filesystem/provider layer;
- hot/warm/cold caching;
- optional remote or secondary-storage backing.

> The goal is not to promise a magical 10× lossless compressor. The goal is to measure how far a transparent storage architecture can realistically go.

## What this project is NOT

- not a magical `100 GB -> 10 GB` compressor;
- not a repack/piracy project;
- not DRM bypass;
- not a ZIP frontend that extracts the whole game before launch;
- not a claim that already-compressed textures, audio and video can always shrink much further.

## Reality check

A universal 10× reduction with byte-identical output, no remote backing and no runtime cost is not realistic for arbitrary modern games. Much game content is already compressed or entropy-dense. The promising direction is **systems engineering rather than a new universal codec**: chunking, deduplication, compression where it actually helps, sparse/virtual files, caching, and optional streaming.

The initial target is a **local-only, byte-identical prototype**. Remote backing and perceptual asset recompression are explicitly later, opt-in research tracks.

## Working architecture

```text
Game / application
       |
       v
Virtual filesystem / provider
       |
       v
Chunk resolver -----> Hot cache
       |
       +-----------> Compressed content-addressed store
       |
       +-----------> Optional secondary / remote backing (later)
```

See [`docs/architecture.md`](docs/architecture.md).

## M0 status

M0 is a feasibility phase. It contains:

- architecture and limits research;
- platform/API selection;
- Experiment 01: Zstd independent-frame random-access benchmark;
- measured baseline results on a synthetic mixed-entropy dataset.

No results from the synthetic dataset should be interpreted as a claim about a particular commercial game.

## Experiment 01

Run:

```bash
cd experiments/01-zstd-random-access
python3 benchmark.py
```

Requirements:

- Python 3.10+;
- `zstd` command line tool in `PATH`.

The experiment compares monolithic Zstd compression with independently compressed chunks at several chunk sizes. Independent chunks model the core trade-off of a future virtual filesystem: slightly worse compression can buy bounded read amplification for random reads.

Results are written to `results/latest.json` and `results/latest.md`.

## Planned product modes

| Mode | Intent | Byte-identical? | Remote backing? |
|---|---|---:|---:|
| Safe | compression + dedup + transparent reconstruction | yes | no |
| Balanced | Safe + removable/regenerable caches and optional content rules | mostly | no |
| Aggressive | optional perceptual texture/audio/video transcoding | no | no |
| Streaming | minimum local working set + on-demand chunks | source chunks remain exact | optional |

## Technology direction

The likely main implementation language after M0 is **Rust**. For a first real gaming-platform filesystem prototype, Windows is the primary target. Candidate provider layers are WinFsp and Windows Projected File System; macOS and Linux follow after the storage format and resolver are stable.

M0 intentionally uses Python + the system Zstd CLI so the first hypothesis can be tested before committing to a large Rust codebase.

## Repository layout

```text
PlaySparse/
├── README.md
├── ROADMAP.md
├── CONTRIBUTING.md
├── SECURITY.md
├── LICENSE
├── docs/
├── experiments/
│   └── 01-zstd-random-access/
├── src/
├── tests/
├── benchmarks/
└── tools/
```

## Safety / legal scope

PlaySparse is for storage research on files the user is authorized to access. The project must not bypass DRM, license checks, anti-cheat systems or platform protections. Experiments must treat source installations as read-only.

## License

MIT.

## M0 measured baseline

The first local run used a **32 MiB synthetic mixed-entropy dataset** at Zstd level 3. It is intentionally not a game corpus.

| Layout | Ratio | Random-read p95* | Sequential reconstruction |
|---|---:|---:|---:|
| 256 KiB chunks | 3.993× | 1.675 ms | 187.2 MiB/s |
| 1 MiB chunks | 3.998× | 3.755 ms | 553.2 MiB/s |
| 4 MiB chunks | 3.999× | 7.170 ms | 1107.4 MiB/s |
| 16 MiB chunks | 3.999× | 18.727 ms | 2397.8 MiB/s |

\* Random-read latency includes spawning the `zstd` CLI for every access, so absolute values are intentionally conservative and are not a production prediction. The useful signal is the chunk-size trend.

See [`experiments/01-zstd-random-access/results/latest.md`](experiments/01-zstd-random-access/results/latest.md) for the complete run metadata.
