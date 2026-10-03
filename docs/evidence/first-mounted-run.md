# First real mounted application execution

**Linux development and hosted Windows native runtime: WORKING. WINDOWS HARDWARE TEST REQUIRED.**

The generated TestGame executable ran from a genuine read-only FUSE mount,
loaded config/assets, read random regions and mapped a 10 GiB logical file. The
original and mounted bytes matched. The mount served compressed CAS callbacks;
the runner did not extract files. After unmount, the mountpoint was empty.

This is a storage-runtime result. No game, Steam, DRM, anti-cheat, WOF, or Windows
gaming compatibility result is inferred from it.

## Provenance

Date: 2026-10-02. Host: macOS arm64, Darwin 25.6, 16 GiB RAM. Actual filesystem
tests: privileged Docker Desktop Linux VM, Debian 12, Linux
6.12.76-linuxkit aarch64, 10 visible CPUs, approximately 7.8 GiB VM RAM, real
`/dev/fuse`. Fixtures and stores were on the VM filesystem, outside the macOS
repository bind mount. Allocated-block figures describe the guest filesystem,
not the physical host SSD or its thin-provisioned Docker disk image.

Rust: pinned 1.99.0. Runtime implementation commit:
`1aa4455084bb66baeea62efe337abe56131b8159`. The final CDC mounted runner was
committed at `26ce7f5df797795ee9aefd39774bb59f4ce79a54`; the final dense frontier
tool at `8045e2f`. Raw reports record commit, dirty working-tree state, commands
and executable SHA-256 where the runner supports them. Initial fixed-chunk,
crash and Experiment 04 runs were made while the implementation was uncommitted
on base `dffb875`; they are retained as development measurements. The final
mounted runs repeat the principal acceptance checks against committed code.

Final Linux executable SHA-256:

```text
playsparse  4c2e930bceebf802bebd905116be1d1e789277f42b61b808f8c03070b895ccd8
io-probe    cc95226c272a24aaed4d73c1da60d092333bbe1e8e887cc48a0b7ef6e811cdc1
```

Binaries are not committed. Later Windows-only publication changes do not alter
the Linux read path.

## Reproduction

On Linux with FUSE mount permission, fuse3, Python 3, an installed Zstd CLI, a C
compiler and the pinned Rust toolchain:

```bash
cargo build --locked --release --workspace
sudo python3 tools/mounted-smoke.py \
  --work /tmp/playsparse-mounted-10g-cdc \
  --playsparse target/release/playsparse --io-probe target/release/io-probe \
  --world-bytes 10737418240 --iterations 64 --cache 64M --chunker cdc
cargo test -p playsparse-vfs-fuse -- --include-ignored --nocapture
sudo python3 tools/open-source-smoke.py --work /tmp/playsparse-open-source \
  --playsparse target/release/playsparse --output /tmp/playsparse-open-source.json
sudo mkdir /tmp/playsparse-enospc
sudo mount -t tmpfs -o size=1M tmpfs /tmp/playsparse-enospc
sudo python3 tools/crash-smoke.py --work /tmp/playsparse-crash \
  --disk-full-dir /tmp/playsparse-enospc
sudo umount /tmp/playsparse-enospc
python3 experiments/04-loose-vs-packfiles/run_benchmark.py \
  --work /tmp/playsparse-exp04-work --output /tmp/playsparse-exp04 \
  --repetitions 3 --iterations 200 --size-mib 64
sudo python3 tools/mounted-frontier.py \
  /tmp/playsparse-exp04-work/corpus /tmp/playsparse-exp04-work/packs-0 \
  --cli target/release/playsparse --reads 200 --runs 3 \
  --output /tmp/playsparse-frontier.json
```

Use new work/output directories: fixture and experiment runners refuse existing
outputs. To reproduce inside the development container, use `/src` as repository
path, `CARGO_TARGET_DIR=/src/target-linux`, and binaries under
`/src/target-linux/release`. The full exact commands for each measured run are
in its raw report. The GitHub runtime workflow additionally performs native
Linux and Windows driver-backed mounted tests and uploads logs/JSON.

## Acceptance results

| Feature | Observed status |
|---|---|
| Rust CAS, raw BLAKE3 identities and Zstd | WORKING |
| Packfiles and sorted index | WORKING |
| Per-object and streaming file verification | WORKING |
| Binary-search range reads, cross-chunk and EOF | WORKING |
| Byte-bounded LRU and concurrent single-flight | WORKING |
| Real virtual mount | WORKING, Linux FUSE and native Windows WinFsp CI |
| Random reads through mount | WORKING |
| Concurrent reads, 1/2/4/8/16 threads | WORKING |
| Memory mapping | WORKING, Linux FUSE and native Windows CI |
| Executable from mounted view | WORKING, Linux FUSE and native Windows CI |
| Windows physical test | REQUIRED |

The additional real FUSE integration test exercises both loose and pack layouts,
1,200-entry paginated directory enumeration, denied writes, mmap and a native C
program executed from the mount. A cold concurrent workload recorded nine
in-flight waits for each layout. Cache unit tests prove that competing readers
load an object once; error/panic paths wake waiters and release loader permits.

Raw evidence: [final CDC mounted run](raw/mounted-10g-cdc/result.json),
[initial fixed mounted run](raw/mounted-10g/result.json),
[real FUSE integration log](raw/fuse-backend-tests.log),
[open-source Zstd execution](raw/open-source-zstd.json),
[crash and corruption results](raw/crash/result.json).

## Generated 10 GiB dataset

Nine files: native test executable, config/readme, assets/audio, fixture metadata
and `world.dat`. The world file is logically 10,737,418,240 bytes with 70 seeded
nonzero regions, including offsets around 4 and 8 GiB. The mounted executable
checks 177 ordinary-read/mmap samples; io-probe additionally compares listings,
stat/open, sequential reads, random reads, boundary/EOF and concurrent workloads.
Every compared read must match exactly; timing never substitutes for equality.

Final CDC target 256 KiB, Zstd level 3, packfiles, cache 64 MiB:

| Measurement | Actual value |
|---|---:|
| Logical source, all files | 10,745,242,903 bytes |
| Source allocated file blocks before pack | 12,697,600 bytes |
| Encoded store including metadata | 3,824,115 bytes |
| Allocated store blocks | 3,850,240 bytes |
| Encoded objects / metadata | 2,637,782 / 1,186,333 bytes |
| Unique objects / reused chunk references | 94 / 10,167 |
| Pack and pre-publication verification | 29.230 s |
| Streaming verification | all 9 files, all logical bytes |
| Mounted provider read errors | 0 |
| Provider object loads / cache hits | 106 / 10,818 |
| Raw chunk-cache hit ratio | 99.030% |
| Concurrent in-flight waits | 17 |
| Final resident chunk-cache bytes | 66,716,877, within 64 MiB |
| Complete runner child CPU | 46.258 s |
| Maximum individual child RSS | 88,121,344 bytes (84.04 MiB) |

Child CPU includes pack, verify, the mount daemon, probe and executable. RSS is
the maximum individual child, not simultaneous total RAM; kernel page cache is
excluded. The 10 GiB source is already sparse: its enormous reduction relative
to logical bytes is **not a game-compression ratio**. Actual allocated source
files occupy about 12.1 MiB; the store occupies about 3.67 MiB.

Mounted self-test BLAKE3:
`b03ffa1948cbcb10d1889fdf260b9b9e208114430597b88f67fa302cffb3bb8e`.
Source identity SHA-256 before/after:
`98645b56a3bc91b327186e2cc9ef5c7d84c38f48e6805f0acd88114e0b466363`.
The source fingerprint hashes paths, sizes, modes, mtimes, data-extent positions
and extent contents; it is not a whole-file SHA-256 of 10 GiB of mostly zeros.

A prior CDC run reported FAIL because `st_blocks` changed by 4 KiB while file
contents, extent positions and metadata remained identical. That failed report
is retained in [raw evidence](raw/source-allocation-false-failure/result.json).
The corrected runner records allocation changes separately from source identity.
The final run passed and still reports `source_allocation_changed: true`.

## Mounted space/performance frontier

This separate dense generated corpus has mixed repetitive/random bytes, duplicate
data and 512 small assets. Source: 69,210,112 encoded bytes, 69,234,688 allocated
bytes including directories. Fixed 256 KiB pack store: 22,779,000 encoded bytes,
22,806,528 allocated bytes including directories: **67.087% encoded savings**.
These synthetic results are not predictions for real games.

Three repetitions, 200 reads per size/repetition. Below are medians of the three
per-run p95 values, in milliseconds. Full p50/p95/p99, opens, CPU, RSS, equality
hashes, amplification and cache metrics are retained in
[final raw frontier](raw/mounted-frontier-final.json). The initial measurement
is also retained in [raw frontier](raw/mounted-frontier.json).

| Configuration | Allocated bytes | 4 KiB p95 | 64 KiB p95 | 1 MiB p95 |
|---|---:|---:|---:|---:|
| Original, file-cache eviction advised | 69,234,688 | 0.140041 | 0.041708 | 0.332375 |
| PlaySparse, eviction advised, chunk cache 0 | 22,806,528 | 0.045959 | 0.329750 | 1.554625 |
| Original, preloaded replay | 69,234,688 | 0.000500 | 0.012542 | 0.178042 |
| PlaySparse, preloaded replay, cache 256 MiB | 22,806,528 | 0.001417 | 0.010625 | 0.149000 |
| WOF/CompactOS | NOT MEASURED | — | — | — |

“Cold” here means a fresh mount, no userspace chunk cache, and file-local
`POSIX_FADV_DONTNEED` before reads. The kernel can retain busy pages; compressed
pack cache and device cache are uncontrolled. **It is not disk-cold.** Warm
measurements replay identical preloaded queries and include kernel cache hits.
Latency times `pread`, excluding open, advice and hash verification. 4 KiB queries
mostly target small assets; 64 KiB/1 MiB queries target the single large file.

Mounted cold provider CPU across the three 64 KiB loops: 0.09 s over 0.149 s
wall, approximately 60.4% of one core; 1 MiB: 1.10 s over 1.027 s, approximately
107.1% of one core. `/proc` accounting has 10 ms ticks and excludes client/kernel
workers. Several short loops record zero ticks; this is below timer resolution,
not evidence of zero CPU cost. Peak daemon RSS: cold up to 11.0 MiB, warm up to
71.5 MiB. Kernel page cache and client memory are additional.

Cold raw-byte amplification: median 1.63× / 4.88× / 2.6075× for 4 KiB / 64 KiB /
1 MiB. Warm raw-cache hit ratios include preload and replay: median 0% / 37.15% /
67.35%; kernel hits do not reach the daemon and are absent from this counter.
Sequential read plus open/hash verification, cache disabled: original
1,305.95 MiB/s, mounted 503.00 MiB/s. Both produced SHA-256
`010feb398cd123ed0f4a8bd45eaf23c644f169271989daed5f367165ee129a60`.

## Loose objects versus packfiles

[Experiment 04 raw results](../../experiments/04-loose-vs-packfiles/results/result.json),
three repetitions, fixed 256 KiB, same dense corpus:

| Metric | Loose | Packs |
|---|---:|---:|
| Median pack time | 1.039 s | 0.198 s |
| Encoded bytes | 22,778,992 | 22,779,000 |
| Allocated file bytes | 25,542,656 | 22,790,144 |
| Directory allocation | 999,424 | 16,384 |
| Filesystem files | 771 | 4 |
| Object lookup p95 | 0.000208 ms | 0.000417 ms |
| Random object read/decode p95 | 0.168709 ms | 0.248750 ms |
| Sequential resolver throughput | 762.98 MiB/s | 847.10 MiB/s |
| Median directory enumeration | 4.088 ms | 0.0316 ms |

Packfiles are the default because they cut publication/metadata overhead and
allocated space and improve this sequential run. Their random object p95 was
**worse**, and that negative result remains visible. Direct resolver throughput
is different from mounted OS throughput and must not be substituted for it.

## Real application and failure handling

The installed open-source Zstandard CLI v1.5.4 was copied into its own test
fixture, packed, mounted and executed from the mounted path. It decoded a
fixture from that mount, producing 2,080,768 identical bytes, SHA-256
`3d557b5be35c06c7d6c227acd0a2c085206e88a54d84c488ba21539675724443`.
Dynamic libraries came from the installed Linux OS. Source/store hashes stayed
unchanged. This is a native CLI compatibility check, not a tested game.

Crash tests actually killed pack during partial object, manifest and commit-marker
writes. No destination was published; incomplete stores were rejected. A fresh
pack to a different destination succeeded. A real 1 MiB tmpfs induced
ENOSPC and no published store. Missing/corrupt objects, manifest, index and commit
marker were rejected. Source full SHA-256 remained
`006536072f4945c4001c142ae2055952cfe0152087dcdd4c4a3f0a528a224a60`.
These are process-crash tests; real power-loss/durability testing remains open.

## Native Windows runtime evidence

On 2026-10-03, [native CI](https://github.com/gogolumo/PlaySparse/actions/runs/37093219918)
passed on hosted Windows Server 2025, build 26100, x86-64, using MSVC and the
pinned WinFsp 2.1.25156 driver/SDK. Branch implementation `9ec716f`; the tested
PR merge commit recorded by the runner is
`164f87682ec50061454c9f813faf064e90713667`. This is actual Windows and driver-backed
I/O in a hosted VM, not a cross-build or a physical desktop/game result.

[Raw native evidence](raw/windows-native/result.json) records PASS, source
unchanged, real filesystem/label `PlaySparse` queried through a resolved directory
handle, normal host exit and absent mountpoint after unmount. All 19 probe
workloads matched original bytes: open/stat, sequential, random 4 KiB/64 KiB/1 MiB
with 1/2/4/8/16 threads, EOF/large boundaries and mmap. The executable ran from
the mount and verified 177 ordinary/mapped samples, BLAKE3
`aa07c936ab81ab48003e95e5554d1ed9147ad078156ffc2360ce26e033d238d4`.
Its hash differs from Linux because generated text uses Windows line endings;
original and mounted hashes match within each platform.

Windows fixed-256-KiB fixture: 10,744,948,048 logical bytes, 7,218,059 encoded
store bytes, 120 unique objects, 40,873 reused references. NTFS allocated size,
WOF, CPU/RSS and disk-cold performance were not measured by this Windows runner;
unavailable allocation/resource fields are null. Probe p95 values are from its
uncontrolled-cache workload, not the controlled Linux frontier: mounted 4 KiB
0.0664 ms, 64 KiB 0.1573 ms, 1 MiB 0.8746 ms, all with one reader thread.
The mount log reports 3,078 cache hits, 108 loads, 96.61% raw-cache hit ratio and
27,190,608 resident bytes within a 64 MiB limit.

CI exposed and fixed three separate problems: incorrect MSI feature IDs,
Windows directory publication replacing an existing empty destination, and generic
ACL rights causing actual `ACCESS_DENIED`. The
[failed access report](raw/windows-access-denied/result.json) is retained.
The runner also now performs a typed volume-handle query and avoids Unix SIGINT
in Windows cleanup. Correctness checks remain enabled.

## Current limits and next engineering step

The read path is correct for the measured workloads. Biggest measured costs are
cache-disabled large mounted reads: chunk decode/verification, amplification and
FUSE transitions. Mounted 1 MiB cold p95 is about 4.7× the native advised-eviction
baseline; sequential throughput is about 38.5% of that baseline. File opens also
cost more through FUSE. Warm cached read latency can approach the native baseline.

Windows WinFsp passed hosted native-driver CI; physical desktop
validation is a separate gate. **WINDOWS HARDWARE TEST REQUIRED** remains until
the latter is recorded. macFUSE was type-checked, not physically mounted. Windows
WOF/CompactGUI comparisons, open-source games, Windows desktop/launcher behavior,
power loss, writable saves/updates/overlays, source timestamps/ACL preservation,
alternate streams, links, non-UTF8 names, compaction and remote tiers are untested
or unsupported. Metadata is capped at 256 MiB; decoded chunks/requests at 16 MiB;
at most eight distinct object loads run simultaneously. The cache limit excludes
manifest/index, in-flight buffers, request buffers and kernel page cache.

Next: physical Windows and WOF
baselines on the same unmodified open application/game. Profile static chunk
sizes and metadata/open costs using those traces before introducing adaptive
policies. Optional fuzz targets compile; no sustained fuzz campaign is claimed.
