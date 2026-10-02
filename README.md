# PlaySparse

**Experimental adaptive virtual storage runtime for very large games.**

Modern games can occupy 100–200+ GB. PlaySparse investigates a different question from ordinary compression:

> Can an unmodified game see the normal files it expects while the bytes underneath are stored in a more efficient, adaptive physical representation?

PlaySparse is **not** claiming a magical `150 GB -> 10 GB` lossless compressor. Transparent filesystem compression already exists, and modern game data is often already compressed. The research target is a storage runtime that can combine content-addressed chunks, on-demand reconstruction, caching, tiering and eventually access-trace-driven policy decisions.

## Current status

PlaySparse is still an R&D project, but it now has two working storage primitives beyond the original compression benchmark:

- **Experiment 02 — fixed chunks vs CDC:** shows why content-defined boundaries can preserve reuse across insertion-heavy updates.
- **Experiment 03 — BLAKE3 CAS:** packs a directory into immutable BLAKE3-addressed, Zstd-compressed chunks and reconstructs it byte-for-byte.
- correctness tests validate official BLAKE3 vectors, deterministic chunking and a full CAS directory round trip;
- GitHub Actions runs the lab correctness suite and smoke experiments.

It does **not** yet mount a real game as a virtual filesystem. That is the next major systems milestone.

## What already exists elsewhere

PlaySparse does not claim novelty for:

- Windows WOF/CompactOS/CompactGUI-style transparent compression;
- btrfs/ZFS/filesystem compression;
- WinFsp/ProjFS virtual filesystems;
- Cloud Files hydration;
- FastCDC/content-defined chunking;
- content-addressed storage;
- game codecs such as Oodle Kraken/Leviathan.

See [`docs/prior-art.md`](docs/prior-art.md).

## Research hypothesis

The candidate differentiator is the **control loop**, not any one primitive:

```text
unmodified game
      |
      v
virtual filesystem
      |
      v
range resolver <--------- access trace
      |                        |
      v                        v
cache / prefetch <------ policy optimizer
      |
      v
content-addressed compressed store
      |
      +---- NVMe / SSD / HDD / NAS / remote tier (later)
```

The long-term hypothesis is that PlaySparse can choose, per file or byte range:

- chunk size;
- codec / compression level;
- compressed vs decompressed cache state;
- prefetch behavior;
- storage tier;

based on observed game I/O, and thereby find a better **space × latency × CPU** Pareto frontier than a static filesystem-compression policy.

That hypothesis is not proven yet.

## M0 baseline — random-access compression

The original synthetic benchmark established the expected chunk-size trade-off: smaller independent chunks reduce random-read amplification while preserving almost the same ratio on that generated dataset. Those numbers are synthetic and are not a claim about GTA, Dota, or any other game.

See [`experiments/01-zstd-random-access`](experiments/01-zstd-random-access).

## Experiment 02 — update reuse

Default synthetic run (`24 MiB`, target `256 KiB`, Zstd level 3):

| Chunker | v2 bytes reused from v1 | Compressed unique store / two versions |
|---|---:|---:|
| fixed offsets | ~33.16% | ~16.86% |
| FastCDC-style | ~92.91% | ~10.89% |

On this insertion-heavy **synthetic** update, CDC recovered content boundaries after the insertion and improved reuse by about **59.75 percentage points**. The Python reference CDC implementation is far too slow for production; the result supports the *storage behavior*, not the implementation performance.

See [`experiments/02-fixed-vs-fastcdc`](experiments/02-fixed-vs-fastcdc).

## Experiment 03 — working content-addressed store

The current lab prototype implements:

```text
source directory
    ↓
content-defined chunks
    ↓
BLAKE3-256 IDs
    ↓
Zstd immutable objects
    ↓
manifest
    ↓
verified reconstruction
```

A generated ~2.97 MB corpus was reconstructed with an identical whole-tree SHA-256. The corpus deliberately contains duplicate/compressible data, so its ~60% synthetic saving is **not** an AAA-game estimate.

See [`experiments/03-blake3-cas`](experiments/03-blake3-cas) and [`docs/storage-format-v0.md`](docs/storage-format-v0.md).

## Lab CLI

Today the research implementation can already pack and verify ordinary directories:

```bash
python3 -m playsparse_lab analyze ./some-directory
python3 -m playsparse_lab pack ./some-directory ./some-directory.playsparse
python3 -m playsparse_lab verify ./some-directory.playsparse
python3 -m playsparse_lab unpack ./some-directory.playsparse ./reconstructed
```

Requirements for the lab implementation:

- Python 3.10+;
- `zstd` CLI in `PATH`.

This is **not** the final production CLI. The production storage engine is planned in Rust with in-process BLAKE3/Zstd and a Windows-first virtual filesystem provider.

## Correctness

```bash
bash tools/check.sh
```

The suite checks:

- official BLAKE3 empty and `abc` vectors;
- deterministic CDC behavior;
- chunk concatenation round trip;
- CAS deduplication;
- per-object BLAKE3 verification;
- per-file SHA-256 verification;
- byte-identical directory reconstruction.

## Next milestone

The next critical milestone is not another compression ratio benchmark. It is a **range-serving virtual filesystem prototype**:

```text
original directory
      ↓
PlaySparse pack
      ↓
compressed CAS
      ↓
virtual mounted/projected directory
      ↓
arbitrary reader receives identical bytes
```

Windows is the primary target. ProjFS and WinFsp must be benchmarked against the actual requirements (random reads, memory mapping, concurrency, large files and launcher behavior) rather than chosen by preference.

## Breakthrough policy

A feature is interesting only when it produces a reproducible, non-dominated improvement versus meaningful baselines. Negative results stay in the repository. Synthetic results never become game claims.

See [`docs/breakthrough-criteria.md`](docs/breakthrough-criteria.md).

## Safety / legal scope

- source installations are read-only inputs;
- PlaySparse does not bypass DRM, anti-cheat or license checks;
- commercial game assets are never committed to the repository;
- destructive source deletion is out of scope until reconstruction and crash safety are mature.

## License

MIT.
