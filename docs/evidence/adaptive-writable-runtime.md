# Adaptive writable runtime sprint evidence

Starting main: `09e90e635c6b5d2423243e02bd7df3f7dd94e638`.
Branch: `feat/adaptive-writable-runtime`. Only generated data was used.
The baseline overlay/trace checkpoint and its fixes were published in PR #2;
hosted Linux and Windows checks passed at `668d70f` before that PR was merged.
The following adaptive/tier changes extend that checkpoint.

## Preserved initial results

[Initial Linux updater result](raw/writable-linux-initial/result.json) records a
real mounted update and remount with source/base unchanged. It copied
5,383,963 base bytes for 124 application write bytes. This whole-file copy-up
cost is intentional evidence of the v1 tradeoff, not a compression improvement.
The initial kernel fstat/fchmod failures and passing native FUSE rerun are kept
in the same directory. Windows startup and callback failures are preserved in
[raw logs](raw/writable-windows-failures); subsequent native CI fixed verbatim
path handling, portable write intent and unbuffered fixture parsing.

[Initial adaptive Linux result](raw/adaptive-linux-initial/result.json) preserves
all three static/adaptive trials and the captured JSONL workload. Each mode had
an empty 4 MiB PlaySparse cache, identical byte hashes and uncontrolled OS/device
caches. Static loaded 60,817,408 raw bytes at a 46.636% demand hit ratio; adaptive
loaded 62,521,344 bytes at 45.141%. Prefetch requests were zero even when enabled.
This exposed dispatcher worker migration and overlapping kernel read windows;
regression tests now cover that behavior. These values were not edited after
fixing the detector. The initial explicit evaluation policy is recorded in its
own result; subsequent comparisons use the exact trace-derived policy.

[Initial tiered Linux result](raw/tiered-linux-initial/result.json) passed genuine
secondary/promotion, offline promoted remount, HTTP ranges and deliberately
corrupt mounted HTTP reads. Source, thin base and secondary stayed unchanged.
The initial run recorded 178 promotions (11,428,403 raw bytes), 186 offline
primary-cache hits, and 188 HTTP object-range requests including corrupt replies.
Its Git SHA and dirty source state are explicit; it is not clean-final-SHA proof.

## Reproduction

From a release build on Linux with real FUSE permissions:

```sh
python3 tools/mounted-update.py --work /tmp/playsparse-update-new
python3 tools/adaptive-smoke.py --work /tmp/playsparse-adaptive-new
python3 tools/tiered-smoke.py --work /tmp/playsparse-tiers-new
```

Each work path must be new. Windows uses the same Python scripts with explicit
`--playsparse target/release/playsparse.exe --io-probe target/release/io-probe.exe`.
The workflow preserves raw evidence on failure too, and its HTTP fixtures are
loopback servers with bounded failure modes, not external services.

The updater compares every visible path, size and SHA-256 after actual writes,
unmount/remount, immutable commit and a reset of a disposable overlay copy.
Adaptive capture verifies mounted bytes against the source, then uses fresh
replay-one processes and alternating mode order. Latency is per range request;
wall/CPU include verification hashing and prefetch shutdown; RSS is each child's
lifetime peak. All trials are retained, including worse results. The mounted
latency samples include ordinary syscall/kernel behavior and differ from replay.
Tiers compare full mounted trees, launch the generated executable, inspect source
labels and exact HTTP requests, and require corrupt remote reads to fail.

## Hardware gates

[macOS environment audit](raw/macos-adaptive-host/environment.json) records
macOS 26.6.2 arm64, Rust 1.99.0 and absent macFUSE package/filesystem bundle.
Portable Rust host tests can run; physical macFUSE mount is **BLOCKED** until the
user installs and approves the driver. No privileged extension was installed.
Physical Windows validation is **BLOCKED_ON_PHYSICAL_WINDOWS** and real game
validation is **NOT RUN**. Hosted Windows Server is a separate evidence class.
The optional `tools/windows-hardware-validation.ps1` harness records environment,
readonly/writable/adaptive/tier workloads and an optional user-owned application;
it never uploads or commits application assets or changes the source install.



## Final Linux and macOS host executions

[Final Linux updater](raw/writable-linux-final/result.json), executed from clean
`1f2fab7`, passed update/remount, new immutable commit/verify/read-only mount and
discard of a disposable overlay copy followed by a base-view mount. Every path,
size and SHA-256 matched. Source and original base stayed unchanged. It measured
5,383,963 copy-up bytes, 124 application write bytes, 5,398,528 allocated overlay
bytes, 1.129 seconds total wall and 1.311 seconds child CPU. The update trace
recorded 1,713 events without drops or writer errors. Mixed-operation trace
quantiles must not be described as application read latency.

[Final Linux adaptive](raw/adaptive-linux-final/result.json) and
[final Linux tiers](raw/tiered-linux-final/result.json) used clean `8a2d46c`.
All workload bytes matched and source/base stayed unchanged. The adaptive run
used the exact saved trace-derived policy, with no post-result parameter tuning.
Its mounted prefetch counters are nonzero and terminal accounting conserves
loaded = useful + wasted, with zero outstanding reservations. The policy is
functional but does not improve this workload's overall cost.

The table contains independently calculated medians across all three recorded
STATIC/ADAPTIVE trials; every raw trial remains available. Each mode started
with the same empty 4 MiB PlaySparse cache. OS and device caches were uncontrolled;
these are not disk-cold measurements. Read amplification divides all verified
raw loaded bytes, including speculation, by returned resolver bytes.

| Metric | STATIC | ADAPTIVE |
| --- | ---: | ---: |
| p50 latency (ns) | 39,041 | 43,542 |
| p95 latency (ns) | 69,083 | 107,500 |
| p99 latency (ns) | 106,292 | 175,166 |
| Demand hit ratio | 0.466360 | 0.512938 |
| Raw bytes loaded | 60,817,408 | 146,341,888 |
| Read amplification | 0.596018 | 1.434168 |
| Prefetch bytes | 0 | 90,832,896 |
| Useful prefetch bytes | 0 | 7,012,352 |
| Wasted prefetch bytes | 0 | 83,820,544 |
| CPU seconds | 0.095391 | 0.170128 |
| Wall seconds | 0.095388 | 0.109349 |
| Process peak RSS bytes | 22,978,560 | 22,978,560 |
| Peak cache bytes | 4,194,304 | 4,194,304 |

This run is a negative result: a higher demand hit ratio came with worse tail
latency, CPU cost and loaded bytes. Static LRU remains the default; experimental
adaptive settings require workload-specific measurement. The byte hashes match.
The [unchanged historical trace replay](raw/adaptive-historical-trace-final/result.json)
also checks the exact preserved initial trace and dataset with the corrected
runtime. Its different timing does not erase the newer losing result or establish
a general benefit; cache/scheduler state is uncontrolled.

[macOS host quality gates](raw/macos-adaptive-quality-final/result.json) passed
fmt, clippy with -D warnings, all 61 portable workspace tests, release build and
5 Python reference tests plus historical experiment checks, from clean `1f2fab7`.
Physical macFUSE remains blocked. Later Windows-only test-module placement and
tier-input prevalidation changes retain the assertions; their native CI checks
are recorded separately. The latter rejects traversal before Windows verbatim
path joining can normalize it away.

[Startup race regression](raw/tier-startup-race/post-fix.json) preserves the old
reproduction (1 promoted object written inside immutable secondary after 10
attempts) and the fixed 1000-attempt result. Shared directory anchors walk every
Unix component without following symlinks; Windows retains each ancestor handle
without delete sharing. Hosted native Windows tests at `4e39b9c` now pass for
that implementation; physical client validation remains required.


## Final hosted Windows execution

[Native Windows CI](https://github.com/gogolumo/PlaySparse/actions/runs/37131829873)
passed both Windows and Linux jobs for branch head `4e39b9c`. The retained
[CI metadata](raw/windows-adaptive-final/ci-run.json) identifies that head;
GitHub checked out PR merge commit `670003162a3b4218b3175761434a8532bc7094d0`,
which each raw report records with an empty working-tree status. This is hosted
Windows Server 2025 build 26100 on AMD64, Python 3.12.10, Rust 1.99.0 and the
checksum-pinned WinFsp 2.1.25156 installer. The installer log is retained.

The [readonly report](raw/windows-adaptive-final/readonly/result.json) proves a
real WinFsp volume, generated native executable, mmap/concurrent comparisons and
10 GiB logical world with 64-bit offsets. The [updater report](raw/windows-adaptive-final/update/result.json)
passes update/remount, immutable commit/verify/mounted tree, disposable-copy
discard/mounted base view, unchanged source and unchanged original CAS. It
records 5,383,963 copy-up bytes, 8,179 callback write bytes, seven overlay files
and nine tombstones. Windows allocated-byte measurement is unavailable, so its
`null` must not be replaced by logical size. All 1,639 update trace events were
written without drops or writer errors.

[Adaptive Windows results](raw/windows-adaptive-final/adaptive/result.json)
verify matching hashes in three fresh-process trials per mode and both actual
mounts, using the exact derived policy. The OS captures 569 resolver requests
on this workload; the Linux trace has 960, so their latency values are separate
platform experiments. Windows mounted p95 was 149,600 ns STATIC versus 153,500 ns
ADAPTIVE, with worse adaptive p99 and wall time too. Prefetch is functional and
bounded; correctness PASS does not imply a performance win. Independent median
metrics are retained in [summary.json](raw/windows-adaptive-final/adaptive/summary.json).

[Windows tiers](raw/windows-adaptive-final/tiers/result.json) pass secondary
promotion (174 objects, 11,141,740 raw bytes), offline promoted remount (180
primary cache hits, no secondary or HTTP request), native executable launch and
exact HTTP ranges. The server handled 181 exact object requests including the
expected corruption failure; one integrity error blocked the damaged object
from reaching the application. Source, base and secondary fingerprints match.

The same run passed fmt, clippy with `-D warnings`, locked workspace tests,
release build and PowerShell harness parsing. Both push and PR workflows are
green at `4e39b9c`. **Physical Windows remains BLOCKED_ON_PHYSICAL_WINDOWS;
physical macFUSE remains BLOCKED; real game validation remains NOT RUN.**


## Final trace import hardening

The final review found that line/event caps alone still allowed oversized
imported identity strings, uncapped distinct worker/path stream keys and
overflowing byte totals. Shared event deserialization now enforces byte limits
for paths, categories and identifiers. Summary caps every retained identity map
and total string-key bytes (32 MiB), and rejects read/write total overflow.
The five new regressions cover oversized fields, 100,001 distinct workers,
retained-key exhaustion, overflowing totals and metadata-operation numeric
compatibility. They supplement the five existing queue/shutdown/sink tests.
This changes trace import validation; mounted callback behavior is unchanged.


[Final macOS host quality gates](raw/trace-quality-final/result.json), executed
from clean `3038c33`, pass fmt, locked workspace clippy with `-D warnings`,
all 66 portable tests, locked release build, 5 Python reference tests and
historical experiments. All 26 preserved Linux/Windows trace summaries remain
identical after the import hardening (14,420 events). Native final-commit checks
are available in the [follow-up PR](https://github.com/gogolumo/PlaySparse/pull/4).
Raw Windows CRLF output, installer output and blank log lines are retained
verbatim; whitespace checks apply to edited source/documentation.
