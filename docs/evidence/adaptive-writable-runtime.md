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

Final runtime reruns and their native CI links are appended after execution.
