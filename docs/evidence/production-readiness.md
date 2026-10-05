# Production readiness validation

Native macOS runtime support was subsequently completed and merged through
[PR #10](https://github.com/gogolumo/PlaySparse/pull/10). The two macOS runtime
fix commits are `c6d7f2a` (single-threaded fuser session on macOS) and `e8c2dea`
(clean Disk Arbitration unmount plus canonical doctor cleanup). Native Apple
Silicon validation with macFUSE 5.4.0 then passed the bounded doctor
mount/read/unmount probe and the full POSIX readonly, writable, adaptive and
tiered generated-fixture suite. Local evidence was retained at
`/private/tmp/playsparse-macos-native-01` on the validation machine; no generated
fixture payload is committed to the repository. This establishes native mounted
macOS software-path evidence, not real-game or launcher compatibility.

The implementation sprint is merged through [PR #7](https://github.com/gogolumo/PlaySparse/pull/7)
into main at `48a0779`. Its tested feature head was `931203f`. Local full
validation used clean `7692d7b`; the subsequent fixes cover Windows structured
doctor output, rooted query paths, deterministic FUSE teardown and CI evidence
staging. All eight final push/pull-request checks passed for `931203f`:
[PR runtime](https://github.com/gogolumo/PlaySparse/actions/runs/37231044516),
[PR research](https://github.com/gogolumo/PlaySparse/actions/runs/37231044520),
[push runtime](https://github.com/gogolumo/PlaySparse/actions/runs/37231041966) and
[push research](https://github.com/gogolumo/PlaySparse/actions/runs/37231041962).
Raw PR artifacts record the actual synthetic merge checkout `e65e7a9`, not
merely the feature head. Timestamps and source/binary hashes remain verbatim.

## Verified software paths

| Check | Result and evidence |
| --- | --- |
| Baseline fmt/clippy/tests/release/research | [PASS](raw/production-readiness-20261004/baseline-host/result.json), before implementation |
| Baseline signed macFUSE SDK | [PASS](raw/production-readiness-20261004/baseline-macos-sdk/result.json); mount NOT RUN |
| Native Mac fmt, locked clippy, workspace tests, release, research | [PASS](raw/production-readiness-20261004/final-host/result.json); 89 Rust tests |
| Signed macFUSE 5.4.0 compile/link/tests and image cleanup | [PASS](raw/production-readiness-20261004/final-macos-sdk/result.json); 99 tests, 2 mount tests ignored |
| Native Apple Silicon doctor mount/read/unmount | PASS after PR #10; exact probe bytes served through a different filesystem device and clean Disk Arbitration teardown |
| Native Apple Silicon readonly/writable/adaptive/tiered POSIX validation | PASS; `tools/posix-runtime-validation.py --work /private/tmp/playsparse-macos-native-01 --build` completed all four generated-fixture stages |
| Linux VM canonical build/doctor/four mounted stages | [PASS](raw/production-readiness-20261004/linux-canonical/canonical-run02/result.json), clean source and binaries unchanged |
| Linux VM fmt/clippy/workspace and actual ignored FUSE tests | [PASS](raw/production-readiness-linux-20261004/checks/result.json); 96 ordinary tests plus 2 driver-backed tests |
| Linux crash/corruption and actual 1 MiB tmpfs ENOSPC | [PASS](raw/production-readiness-linux-20261004/crash/result.json); destination absent and source unchanged |
| Loose/packfile experiment 04 | [PASS](raw/production-readiness-linux-20261004/experiment04/result.json); 32 MiB, 2 repetitions, 64 iterations |
| Owned-app harness with generated executable classified as game | [PASS software](raw/production-readiness-20261004/linux-canonical/owned-fixture01/result.json), real game NOT RUN |
| Hosted native Windows canonical WinFsp runtime | [PASS software](raw/production-readiness-20261004/ci-final-931203f/windows/result.json); physical BLOCKED, game NOT RUN |
| Hosted original/verified WOF/PlaySparse read comparison | [PASS software](raw/production-readiness-20261004/ci-final-931203f/windows/wof-system-volume/result.json); physical WOF BLOCKED |

The SDK run checks the pinned checksum, Apple installer signature and
notarization, attaches read-only, and uses extracted temporary libraries. It
installs no driver. The earlier Mac doctor
[blocked probe](raw/production-readiness-20261004/macos-doctor/result.json) is
retained as historical evidence from before macFUSE installation/approval. After
PR #10, native Apple Silicon doctor validation passed with macFUSE 5.4.0: exact
probe bytes were read through an observed different filesystem device, the mount
detached cleanly, and no stale PlaySparse mount remained. The full native macOS
POSIX suite then passed readonly, writable/remount, adaptive and local/HTTP-tier
generated-fixture stages. On Linux, the bounded tiny probe likewise verifies
exact bytes and a different filesystem device before the 10 GiB readonly,
writable/remount/commit/discard, adaptive and local/HTTP-tier stages.

The generated owned-app self-test exited 0. Source, base, binary and repository
identities remained unchanged and its mount detached. Passing `--application-kind
game` did not promote a detected generated fixture to real-game evidence.
Applications run with literal argument vectors and writes go to a separate
overlay. The process is not an OS sandbox; external application I/O remains an
application behavior to validate. The POSIX helper reaps its owned process group;
Windows uses taskkill for live child trees and checks its known owned mounts.
No guaranteed cleanup of arbitrary orphaned Windows descendants is claimed.

## Retained failures and corrections

The audit captured [spawn failure leaving RUNNING](raw/production-readiness-20261004/runner-spawn-failure/audit-result.json),
[SIGTERM leaving a child alive](raw/production-readiness-20261004/runner-sigterm-failure/audit-result.json)
and [pack creating directories inside its source](raw/production-readiness-20261004/pack-source-failure/result.json).
Regression tests now cover those cases, atomic report ENOSPC, timeout cleanup,
source/base changes after failed launches, dirty/receipt mismatches, reused work,
symlinks, literal arguments and generated-game classification. The pack fix
checks a destination's projected canonical parent before creating directories.

A validation run interrupted by concurrent checkout changes correctly retained
[FAIL](raw/production-readiness-20261004/linux-canonical/canonical-run01/result.json)
and issued no matching build receipt. The clean repeat passed. An initial
[clippy test assertion failure](raw/production-readiness-20261004/clippy-test-assertion-failure.json)
was corrected before final validation.

Initial CI failures are retained in [Linux log](raw/production-readiness-20261004/initial-ci-failures/playsparse-readiness-ci-linux-artifact-failure-7692d7b.log)
and [Windows log](raw/production-readiness-20261004/initial-ci-failures/playsparse-readiness-ci-windows-failure-7692d7b.log).
Linux software stages passed but upload lacked access to private root-owned
work; CI now archives only evidence for upload. Windows query validation now
rejects current-drive rooted paths, and the mocked Linux mount-table test uses
Linux path semantics. Later CI waits for complete FUSE teardown before remount,
uses structured WinFsp doctor data, and runs its disposable WOF fixture on the
Windows Server system volume. Only JSON/log evidence is staged back to the
runner's data volume; generated assets and compressed copies remain local.

## Performance limits

[Adaptive evidence](production-readiness-policy.md) retains all twelve exact-input
BEFORE/AFTER runs and their dirty scoped provenance. AFTER adaptive reduced raw
loads from 153,354,240 to 62,521,344 bytes, but still loses to same-build static:
p95 47.458 → 52.083 µs, CPU 0.082633 → 0.088748 seconds. Static remains default.

The hosted WOF comparison uses a deliberately repetitive 7,200,028-byte generated
fixture, 300 identical requests and three rotating-order warm trials per mode.
WofIsExternalFile verifies XPRESS4K provider/algorithm/flags; original and sealed
base fingerprints remain unchanged. Median p95 original/WOF/PlaySparse is
244.0/247.1/238.8 µs, while median total workload time is
0.035399/0.036670/0.036097 seconds. These mixed synthetic warm results establish
no general advantage or physical-game result. OS/device caches are uncontrolled;
application startup is NOT RUN. Allocation sums file data streams and excludes
filesystem metadata. Native/WOF kernel amplification and absent driver-error
counters are null. All trials, CPU/RSS, storage values and request identities
remain in the raw result; no fastest sample is selected.

## Physical and compatibility gates

| Gate | Status |
| --- | --- |
| Native mounted Mac runtime | PASS: Apple Silicon + macFUSE 5.4.0 generated-fixture doctor/readonly/writable/adaptive/tiered validation |
| Physical Linux storage/performance | HARDWARE REQUIRED: available Linux validation is a VM |
| Physical Windows desktop | HARDWARE REQUIRED: hosted WinFsp is separate evidence |
| Physical Windows original/WOF/PlaySparse | HARDWARE REQUIRED: hosted generated comparison only |
| Owned real game | GAME EVIDENCE REQUIRED: no workload supplied |
| Steam/Epic/other launcher | NOT RUN: direct generated execution is not launcher testing |

Next on the Mac is representative owned-application validation, followed by an
owned game without DRM/anti-cheat if one is supplied. The canonical native
runtime command remains:

```bash
python3 tools/posix-runtime-validation.py --work /private/tmp/playsparse-user-mac-02 --build
```

For an owned application, use the POSIX runner's `--source` and `--executable`
arguments as documented in the [POSIX guide](../posix-validation.md). Each work
path must be new. Passing generated fixtures is not launcher, Steam/Epic,
DRM/anti-cheat or commercial-game evidence. The [Windows comparison
guide](../windows-comparison.md) remains the path for a physical Windows client.

`origin/docs/macos-first-readme`, `origin/feat/adaptive-writable-runtime`,
`origin/feat/macos-posix-runtime` and `origin/runtime-native-check` were verified
merged into main. They are cleanup candidates; none was deleted.

[Raw collection](raw/production-readiness-20261004/collection.json) contains exact
selected evidence copies. No corpus, sealed store, overlay, executable, SDK or
installer payload is committed. Separately, 218 unexpected untracked numbered
files were reversibly moved outside Git with a restore manifest at
`/tmp/playsparse-readiness-untracked-duplicates-20261004-01/manifest.json`.
Their contents were not rewritten/deleted and no byte-identity claim is made.
