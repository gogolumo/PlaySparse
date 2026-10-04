# PlaySparse

**Experimental adaptive virtual storage runtime for very large games.**

Modern games can occupy 100–200+ GB. PlaySparse investigates a different question from ordinary compression:

> Can an unmodified game see the normal files it expects while the bytes underneath are stored in a more efficient, adaptive physical representation?

PlaySparse is **not** claiming a magical `150 GB -> 10 GB` lossless compressor. Transparent filesystem compression already exists, and modern game data is often already compressed. The research target is a storage runtime that combines content-addressed chunks, on-demand reconstruction, caching, tiering and access-trace-driven policy decisions.

## Current status

The Rust runtime serves file ranges directly from compressed CAS through real
Linux FUSE and Windows WinFsp mounts. Generated native programs and the
open-source Zstd CLI have run from the mounted view. A generated 10 GiB file
passed mounted reads beyond 4/8 GiB, mmap and concurrent I/O. Hosted Windows
Server 2025 CI passed the read path, writable updater/remount/commit/reset,
adaptive capture/replay and tiered/HTTP mounted paths at `4e39b9c`
([native CI evidence](https://github.com/gogolumo/PlaySparse/actions/runs/37131829873)).

The optional persistent writable overlay, generated updater, bounded JSONL
tracing, adaptive cache/prefetch policy, local tiers and verified HTTP object
ranges are implemented as **EXPERIMENTAL** capabilities. Linux driver-backed
tests cover writable operations and updater/remount behavior, traced byte
comparisons, and tiered reads/failures. These development and hosted-CI results
do not establish compatibility with a physical gaming desktop or a launcher.

### Current capabilities

- [x] **WORKING:** directory → Rust BLAKE3/raw/Zstd CAS; originals remain untouched
- [x] **WORKING:** deterministic v1 manifests, indexed packfiles and loose comparison layout
- [x] **WORKING:** per-object and streaming whole-file verification
- [x] **WORKING:** binary-search byte-range reads; only intersecting chunks loaded
- [x] **WORKING:** byte-bounded LRU, metrics and concurrent single-flight loads
- [x] **WORKING:** Linux FUSE mount, normal reads, directories, mmap and executable launch
- [x] **WORKING:** analyze, pack, verify, mount, unmount, benchmark, doctor and io-probe
- [x] **WORKING:** generated 10 GiB mounted test corpus and reproducible evidence
- [x] **WORKING:** native WinFsp mounted I/O and executable launch in hosted Windows CI
- [x] **EXPERIMENTAL:** persistent writable overlay, stable open handles, remount, status/discard/commit
- [x] **EXPERIMENTAL:** generated mounted updater; native Windows overlay baseline passed hosted CI
- [x] **EXPERIMENTAL:** bounded JSONL tracing and trace summaries
- [x] **EXPERIMENTAL:** versioned policy, decaying hotness, bounded sequential prefetch and static/adaptive replay
- [x] **EXPERIMENTAL:** verified secondary local objects and persistent promotion cache
- [x] **EXPERIMENTAL:** HTTP 206 object ranges; corrupt remote reads fail
- [ ] **HARDWARE REQUIRED:** physical Windows desktop validation
- [ ] **GAME EVIDENCE REQUIRED:** real game and real launcher compatibility
- [ ] **HARDWARE REQUIRED:** Windows original/WOF/PlaySparse comparison
- [ ] **HARDWARE REQUIRED:** physical macFUSE mount; kernel transport compiled against the verified 5.4 SDK, experimental

Adaptive performance benefit is unproven. The retained initial Linux benchmark
issued zero prefetch requests and had a lower adaptive cache hit ratio than the
static baseline. The corrected detector activates bounded prefetch, but the final
three-trial replay still loses: median p95 69 → 108 µs, CPU 0.095 → 0.170 s,
and raw bytes loaded 61 → 146 MB. All trials and the exact policy are retained;
static LRU remains the default.

**WINDOWS HARDWARE TEST REQUIRED.** The production Windows milestone remains open.
See [first mounted run](docs/evidence/first-mounted-run.md),
[adaptive writable runtime evidence](docs/evidence/adaptive-writable-runtime.md),
[writable overlay](docs/writable-overlay.md),
[adaptive policy](docs/adaptive-policy.md),
[tiered storage](docs/tiered-storage.md),
[FUSE backend](docs/fuse-backend.md), [macOS/Red Hat validation](docs/posix-validation.md),
[Windows backend](docs/windows-backend.md),
[format v1](docs/storage-format-v1.md) and
[Experiment 04](experiments/04-loose-vs-packfiles).

Experiments 01–03 and the Python `playsparse_lab` remain research/reference
implementations. Their earlier reconstruction and synthetic reuse results below
are preserved; production reads use Rust in-process BLAKE3/Zstd.

## Rust CLI

Build with the pinned Rust toolchain, then put `target/release` on PATH:

```bash
cargo build --locked --release --workspace
playsparse doctor
playsparse analyze ./TestGame
playsparse pack ./TestGame ./TestGame.playsparse
playsparse verify ./TestGame.playsparse
mkdir ./mounted
playsparse mount ./TestGame.playsparse ./mounted --cache 256M --trace ./reads.jsonl
```

Linux requires FUSE (`/dev/fuse` and mount permission or `fusermount3`). In a
second terminal, ordinary programs can read the virtual files:

```bash
cat ./mounted/readme.txt
./mounted/testgame --self-test
io-probe ./TestGame ./mounted --iterations 64 --output io-probe.json
playsparse unmount ./mounted
playsparse benchmark ./TestGame ./TestGame.playsparse --output benchmark.json
```

`pack` defaults to packfiles, CDC target 256 KiB (64 KiB–1 MiB chunks), and Zstd
level 3, retaining raw objects if compression expands them. `--chunker fixed`
and `--layout loose` remain measured baselines. Cache accepts `64M`, `256M`, `1G`
or integer bytes. Mount stays in the foreground. Stores must stay immutable.
Omitting `--overlay` keeps the mount read-only.

To enable writable files and directories, use a separate overlay and trace
destination. Start the mount in one terminal:

```bash
playsparse mount ./TestGame.playsparse ./mounted --cache 256M \
  --overlay ./TestGame.overlay --trace ./update.jsonl
```

Run the application or updater through `./mounted`, then manage the inactive
overlay after unmounting:

```bash
playsparse unmount ./mounted
playsparse overlay status ./TestGame.overlay
playsparse overlay commit ./TestGame.playsparse ./TestGame.overlay ./TestGame-updated.playsparse
playsparse verify ./TestGame-updated.playsparse
playsparse overlay discard ./TestGame.overlay
```

`commit` writes a new store and leaves the base intact. It first streams the
entire merged tree into a disposable staging directory: enough temporary disk
space for its full logical size, plus the new encoded store, is required.
`discard` resets the overlay's changes. A one-byte edit to a base file can also
copy that whole file into the overlay; see the [copy-up limits](docs/writable-overlay.md).

Summarize a read-only trace, generate a policy, and compare it with static LRU
using identical recorded requests:

```bash
playsparse trace summarize ./reads.jsonl
playsparse optimize ./reads.jsonl --output ./policy.json
playsparse replay ./TestGame.playsparse ./reads.jsonl --cache 256M \
  --policy ./policy.json --repetitions 3 --output ./replay.json
playsparse mount ./TestGame.playsparse ./mounted --cache 256M \
  --policy ./policy.json --tiers ./tiers.json --trace ./tiered-reads.jsonl
```

Create `tiers.json` using the [versioned local/HTTP schema](docs/tiered-storage.md).
Policy and tiers are independent optional mount flags; they can also accompany
`--overlay`. Trace destinations and writable tier caches must be outside the
base, overlay and mount. Replay accepts base-file read traces; it cannot replay
mutable overlay versions. Fresh-process CPU/RSS comparisons use `replay-one`;
neither replay command establishes a cold physical-disk baseline.

macOS builds CAS/range/CLI without a driver; actual mount requires macFUSE and
`cargo build --release --features macfuse`. Windows uses WinFsp with an installed
driver and SDK; choose an unused drive letter or a nonexistent directory path,
following [native Windows commands](docs/windows-backend.md).

`analyze` performs two full measured temporary pack passes (fixed and CDC), with
exact duplicate/reuse and encoded-size results; it makes no sampled game-saving
prediction. Benchmark JSON labels cache state explicitly: chunk-cache-cold is
not disk-cold. Physical allocated blocks and encoded store bytes are separate.

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
      +---- primary local / secondary local / HTTP objects
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

This remains the Python research CLI. The Rust production path above uses
in-process BLAKE3/Zstd and platform filesystem callbacks. V0 and v1 stores are
different formats; repack from source to migrate.

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

The Linux development implementation now proves this **range-serving virtual
filesystem path**:

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

Windows remains the primary target. WinFsp was selected because its read callbacks
return bytes directly, while ProjFS's file-data contract materializes retrieved
bytes in local files. Hosted Windows native mount, large/mapped/concurrent reads
and executable launch have recorded evidence. Physical desktop/game validation
and WOF comparisons remain open. Writable operations and the first
trace/policy/tier implementations now extend that runtime; their experimental
status and measured limitations are recorded in the
[adaptive writable sprint evidence](docs/evidence/adaptive-writable-runtime.md).

## Breakthrough policy

A feature is interesting only when it produces a reproducible, non-dominated improvement versus meaningful baselines. Negative results stay in the repository. Synthetic results never become game claims.

See [`docs/breakthrough-criteria.md`](docs/breakthrough-criteria.md).

## Safety / legal scope

- source installations are read-only inputs;
- PlaySparse does not bypass DRM, anti-cheat or license checks;
- commercial game assets are never committed to the repository;
- destructive source deletion is out of scope until reconstruction and crash safety are mature.

## License

PlaySparse's own code is MIT. The Windows build also links the GPL-3.0 WinFsp
Rust bindings and uses the separately licensed WinFsp driver. Review the
[Windows dependency licensing notes](docs/windows-backend.md#dependency-licenses)
before distributing a combined Windows executable.
