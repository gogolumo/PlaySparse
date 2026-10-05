# Experimental game-aware storage

Game awareness is an **offline research layer**. Optional engine discovery feeds
a versioned PlaySparse profile; measured probes produce an independent packing
plan. The packed result is an ordinary immutable v1 store. `mount`, RangeResolver,
FUSE, macFUSE and WinFsp neither invoke nor require Python/universal-modder.

Universal-modder currently helps discovery, not measured compression quality.
Engine confidence is a signature score, not proof of identity, ownership,
compressibility or game compatibility. See the [pre-code audit](game-awareness-audit.md).

## Commands

Existing `analyze SOURCE`, `pack SOURCE STORE` and all runtime defaults retain
their generic behavior. Profiles/plans are opt-in and live outside the source
and immutable store. Outputs are exclusive: existing files are never replaced.

```sh
playsparse inspect-game /path/to/Game --output game-profile.json
playsparse analyze /path/to/Game --profile game-profile.json --plan-output packing-plan.json
playsparse pack /path/to/Game /path/to/Game.playsparse --plan packing-plan.json
playsparse verify /path/to/Game.playsparse
```

`inspect-game` prints a JSON report containing `profile` and measurement wall/CPU
times; `--output` writes only the versioned profile. `--scanner auto` falls back
only when `um` is absent. `generic` never spawns a scanner; `universal-modder`
returns an actionable absence/error instead. `--scanner-program` names a trusted
executable, not a shell expression. Arguments are literal `scan SOURCE --json`.
The child has a 45-second deadline and 1 MiB limits on each output stream.
Unix cleanup kills/reaps its owned process group; Windows uses a kill-on-close
job. `UM_NO_UV=1` disables upstream's implicit dependency installation and
`PYTHONDONTWRITEBYTECODE=1` disables Python bytecode writes. No installs, launches,
network knowledge refresh, loaders or game modifications are requested.
External executables are user-trusted programs, not a sandbox.

`analyze --profile` retains the two full fixed/CDC temporary-store measurements
and adds a third verified candidate. Numbers describe the actual input, not a
size extrapolation. `already_compressed_bytes` stays null. Candidate file bytes
include unsampled regions and are explicitly **not** measured already-compressed
bytes. Probe CPU/time covers the Rust analysis process; scanner-child CPU is not
silently attributed to it. Experiment 05 measures child CPU for its full commands.

To generate profile and plan without packing temporary analysis stores:

```sh
playsparse inspect-game /path/to/Game --scanner generic \
  --output game-profile.json --plan-output packing-plan.json --container-aware
playsparse pack /path/to/Game /path/to/Game.playsparse --plan packing-plan.json
```

`pack --profile PROFILE [--container-aware]` can derive the same plan in memory
before starting the pack transaction; use a sidecar plan for review/provenance.
V1 plans require CDC, 256 KiB target and Zstd level 3. They permit packs or loose
layout; changing global chunk/codec parameters with a plan is rejected.

## Measured probes and conservative defaults

Files are hashed in full for source identity. Up to three 64 KiB windows at
start/middle/end are measured with Zstd level 3. Records include actual encoded
sample lengths, BLAKE3, entropy in millibits per byte, duplicate sample count and
sample FastCDC chunk count. Extension/header classes are descriptive hints for
code, text, metadata, media, caches, binary and containers. None proves compression.
Small files never qualify for skipped compression.

`PackingPlan` normally retains `try-zstd`: every new object is compressed and
falls back to raw when Zstd is larger, exactly like the generic writer. The
negative experiment motivated a separate `--experimental-skip-compression`
opt-in on inspect/analyze/pack. It sets `experimental_skip_compression` in the
plan. Only files at least 192 KiB whose three distinct probes all fail to shrink
and have at least 7.9 bits/byte sample entropy qualify for `measured-raw`.
This avoids subsequent codec calls but **does not prove unsampled bytes are
incompressible**. Its CPU/space frontier was worse on retained L0 workloads.
Byte identity remains verified even after false classification. This policy is
not recommended and is never silently enabled by a profile or engine label.

## Original-byte ZIP boundaries

`--container-aware` accepts ZIP local records only after a bounded central/local
cross-check. Requirements: single disk, non-ZIP64, no encryption/descriptors,
stored/deflate entries, UTF-8 safe names, consistent lengths/flags/CRC fields,
contiguous local records starting at byte zero, exact central/EOCD coverage.
At most 4096 entries and 8 MiB central metadata are accepted. Embedded ZIPs,
unsupported extras and malformed containers fall back to generic CDC.

The parser never inflates or extracts entries, authenticates their payload CRC,
decodes assets, or rewrites proprietary game data. It identifies structural
byte boundaries only. CDC resets at each original record and the metadata tail;
large records are still CDC-chunked within the existing 1 MiB maximum at the
256 KiB target. All bytes, including original compression and archive metadata,
remain byte-for-byte unchanged. Existing v1 chunk tables already express these
ranges; no format change or profile dependency is introduced in the runtime.

No Unreal/Unity/PCK/VPK/BSA/RPF/REDengine/FromSoft parser is claimed: only hints
exist for those names/headers. ZIP-generated evidence is not PK3 game execution.

## Profile/plan v1 contract

All public records use serde `deny_unknown_fields`; unknown versions and unsafe
paths fail. JSON input is limited to 16 MiB; analysis to 8192 total file/directory
entries, 1024-byte relative paths, three probes/file and bounded container tables.
Modes use the existing store's POSIX/read-only mapping, including Windows 0644
for writable files. Source identities are portable only when bytes, paths and
captured modes match; re-inspect on a different installation/OS when they differ.

Profiles contain `schema_version`, `generated_by`, null `generated_at`, scanner
status/truncation, normalized game/engine/executable/anti-cheat hints,
`source_identity`, analyzed files and empty startup hints. Source identity is
BLAKE3 over a domain-separated sorted inventory of directory paths and file
paths, sizes, modes and full-content BLAKE3. The source root is never persisted.
Free-text scanner evidence, routes, saves/Workshop paths and loader metadata are
discarded; evidence paths survive only when matched to actual source-relative
files. Unknown/multiple engines remain advisory and cannot select a codec.

Plans contain `schema_version`, `generated_by`, source identity, explicit
container/skip flags and sorted per-file path/size/mode/hash, class/hint,
measurement, strategy/target/level, original end boundaries and reason. Imported
measurements, flags and offsets are not trusted: **pack recomputes the complete
analysis and compares the entire plan before creating destination parents**.
It then checks streamed file hashes/sizes/modes against the plan before publication.
Content-derived artifacts are deterministic; run timestamps/CPU/wall times belong
to separate reports. There is no signed identity/ownership attestation.

Runtime priors are deliberately empty. Actual access traces remain authoritative;
static LRU and adaptive opt-in behavior remain unchanged. No startup improvement
or game/launcher support is inferred from engine discovery.

## Evidence and attribution

Use [experiment 05](../experiments/05-game-aware-packing/README.md) and its retained
raw evidence for exact medians, variance and negative results. Update metrics
count missing v2 object payloads plus complete v2 metadata: a potential shared-CAS
byte budget, **not an implemented incremental store publication/allocated update**.
OS/device caches are uncontrolled; cold means disabled userspace chunk cache.
L1 open game-like and L2 owned games, startup and mounted performance remain
separate unrun gates. No industry or breakthrough claim follows from L0.

Research uses [universal-modder](https://github.com/rehan-remade/universal-modder)
at `0f5dcdfdcd8ed420f8413815bd6647586ab894a2`, MIT by Rehan and contributors.
The adapter is original PlaySparse code, not a vendored runtime dependency.
See [third-party notice](../THIRD_PARTY_NOTICES.md).
