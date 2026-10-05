# Adaptive prefetch readiness evidence — 2026-10-04

Static remains the default. The admission and scheduling changes substantially
reduce wasted prefetch on this synthetic trace, but adaptive still has higher
median p95 latency, CPU and raw bytes loaded than static in the same AFTER
build. This evidence does not establish a workload speedup or game support.

The [raw summary](raw/production-readiness-policy-20261004/summary.json) retains
both negative BEFORE results and every AFTER trial. Each phase runs three
trials per mode in alternating order: static/adaptive, adaptive/static,
static/adaptive. Every trial uses a fresh `replay-one` child process and an empty
4 MiB PlaySparse cache. OS and device caches are uncontrolled. Measurements are
from a LinuxKit aarch64 Debian VM with genuine Linux FUSE available; they are
not physical Linux results. BEFORE captured the trace through a FUSE mount;
AFTER replays the exact trace directly through the range resolver.

| Median of three trials | BEFORE static | BEFORE adaptive | AFTER static | AFTER adaptive |
| --- | ---: | ---: | ---: | ---: |
| p95 latency, µs | 41.833 | 51.875 | 47.458 | 52.083 |
| Process CPU, seconds | 0.078280 | 0.141952 | 0.082633 | 0.088748 |
| Process lifetime peak RSS, bytes | 24,162,304 | 24,162,304 | 22,921,216 | 22,921,216 |
| Raw bytes loaded | 60,817,408 | 153,354,240 | 60,817,408 | 62,521,344 |
| Read amplification | 0.596018 | 1.502890 | 0.596018 | 0.612717 |
| Prefetch bytes loaded | 0 | 95,289,344 | 0 | 3,997,696 |
| Prefetch useful bytes | 0 | 4,194,304 | 0 | 3,997,696 |
| Prefetch wasted bytes | 0 | 91,095,040 | 0 | 0 |

The AFTER adaptive/static ratios are approximately 1.097 for p95 latency,
1.074 for CPU and 1.028 for raw loads. Whole-object usefulness counts an object
once demand claims it; it does not claim every byte in that object was read.
Read amplification counts verified raw object bytes loaded divided by bytes
returned to the caller, including speculative loading. Cache reuse can make
that ratio less than one. The table compares medians, so accounting identities
must be checked in each raw run rather than by summing independent medians.

All twelve runs returned 102,039,552 bytes for the same 960 queries and the same
content BLAKE3:
`e93660bc9c1abf1d69a3110bc62fd9b29e617f3cce4baa33a03e14836b25a8c8`.
Each run passed terminal prefetch conservation with zero outstanding
reservations. AFTER verifies the sealed base, trace and policy remain unchanged.

## Inputs and provenance

The comparison reuses the original sealed base at
`/tmp/playsparse-readiness-policy-before-20261004-01/base` inside the Linux VM.
It does not repack, recapture or optimize a new policy for the AFTER runs.
The retained [capture trace](raw/production-readiness-policy-20261004/before/capture.trace.jsonl)
has SHA-256
`4b9619621eeb8c8f6a6e71433453d4b7a4139bdae16e0e222ebcb127af054288`;
the retained [policy](raw/production-readiness-policy-20261004/before/policy.json)
has SHA-256
`0e52665418a64e6262ee380a254f95a98982f346c66beca90b28a55cb6ee1eaa`.

BEFORE and AFTER were recorded at revision `e7d90c7` with a dirty working tree.
The [BEFORE source record](raw/production-readiness-policy-20261004/before/phase8-source.json)
recovers adaptive file hashes from committed bytes because the recorded working
tree status showed those files unmodified. The
[AFTER source snapshot](raw/production-readiness-policy-20261004/after/scoped-source-during-build.json)
records live scoped hashes during its release build. Neither record is a clean
whole-binary source attestation. Other sprint edits were present, and a test
assertion and formatting were corrected after measurement. Raw timestamps and
working tree status are preserved verbatim.

BEFORE binary SHA-256:
`4fd440e7e628db42fafed88a7932046eac073a5f8cf542aeed0ba5569e140da6`.
AFTER binary SHA-256:
`be31aa0eb482c04f6d7d2a03a17d8f8828bd122176ca41322d6715b2801f8594`.
The AFTER build finished in 11.49 seconds according to the retained
[release build log](raw/production-readiness-policy-20261004/after/linux-release-build.stderr.log).
The executable was copied to its owned scratch directory before the six runs,
so a later workspace build could not replace it during measurement.

Exact argument vectors, external timings, input digests and each measurement
are in [BEFORE result](raw/production-readiness-policy-20261004/before/result.json)
and [AFTER result](raw/production-readiness-policy-20261004/after/result.json).
The sealed synthetic store is retained locally rather than committed; the
repository retains traces, logs, policy and content digests. Replaying the
retained trace requires that same sealed store. Generating a new corpus is a
new experiment and must retain its own identities and results.

## Correctness and gates

Focused locked tests pass: 15 cache, 6 policy and 10 range tests. Locked clippy
with all targets and warnings denied passes; results and stdout/stderr are in
[checks](raw/production-readiness-policy-20261004/checks/result.json).
The tests exercise mixed-size multi-victim admission, entry pressure,
pre-load/post-load demand races, full demand loader slots, source attribution,
bounded failed-attempt history, tiny sequential bursts, exact content and
prefetch accounting. The policy algorithm and its limits are described in
[adaptive-policy.md](../adaptive-policy.md).

| Gate | Result |
| --- | --- |
| Exact-input Linux VM replay and byte equality | PASS |
| Lower AFTER speculative waste on this trace | PASS |
| Adaptive faster than same-build static on this trace | FAIL |
| AFTER mounted filesystem/application benchmark | NOT RUN in this comparison |
| Physical Mac with approved macFUSE | BLOCKED: driver unavailable |
| Physical Linux workload | BLOCKED: physical runner unavailable |
| Physical Windows/WOF workload | BLOCKED: physical runner unavailable |
| User-owned game or launcher | NOT RUN: no workload supplied |

This is a regression and waste-reduction experiment on one synthetic access
pattern. Continue using static unless repeatable evidence on the intended
workload supports an explicit adaptive policy.
