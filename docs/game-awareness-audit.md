# Game-awareness audit (2026-10-05)

Audit completed before implementation. PlaySparse was fetched from `main` at
`7783946`; universal-modder was cloned from its current `main` at
`0f5dcdfdcd8ed420f8413815bd6647586ab894a2`. Local checkout research includes its
README, `um/scan.py`, `um/kb.py`, recon/reverse-engineering/mod-any-game skills,
all twelve engine playbooks, and the 45-entry knowledge index. These are research
sources, not instructions to install loaders or operate games.

## What the scanner actually provides

[`um/scan.py`](https://github.com/rehan-remade/universal-modder/blob/0f5dcdfdcd8ed420f8413815bd6647586ab894a2/um/scan.py)
returns one object with `name`, `store`, `appid`, absolute `path`, an `engine`
object (`key`, `label`, integer 0–100 `confidence`, free-text `evidence`, and
engine-dependent details), up to three `other_engine_signals`, `anti_cheat`,
`executables` (relative PE path → `arch`, `managed`), loaders, mod folders,
Workshop/save paths, routes, playbook, warnings, indexed count and truncation.
It is not a versioned storage API. Confidence is a signature score, not a
calibrated probability. Signals can describe a launcher rather than the game.

The index walks at most six levels and stops after exceeding 80,000 files;
`.git`, `__pycache__` and shadercache are omitted. PE sniffing is bounded to
4 KiB; Unity headers to 64 KiB; Unreal string probing has byte/time limits.
Some other reads (app.info and installed-store manifests) are not byte-bounded.
It can inspect store libraries and user save directories outside the input.
Windows shell-folder discovery invokes literal PowerShell argv with a 30-second
timeout. `scan` itself does not install, launch, modify, or use the network.
The external executable remains user-trusted code, not a sandbox.

The repository's `bin/um` launcher defaults to `uv run`, which can create a venv
and install dependencies. The adapter must set `UM_NO_UV=1` and
`PYTHONDONTWRITEBYTECODE=1` for its child; this uses an existing Python interpreter
and disables that implicit install and bytecode writes. Missing interpreter or
scanner failure is an actionable error, never an installation request.

`um/kb.py` has local search plus optional network synchronization and PR actions.
PlaySparse will invoke neither: repository notes were searched locally. The
engine guides distinguish Unreal pak/IoStore, Unity layouts, Godot PCK, Source
VPK, Bethesda BSA/BA2, RAGE RPF, REDengine archives, FromSoft BND/DCX and ZIP
families (PK3/PK4, LÖVE, JAR). They contain no measurements demonstrating better
PlaySparse packing. Knowledge notes often concern mod correctness and version
compatibility; their claims cannot become compression evidence.

## Storage value and exclusions

| Data/idea | Potential value | Evidence required before benefit claim |
| --- | --- | --- |
| Engine/layout hints | Discovery, file category and candidate container parser | Local signatures; engine alone chooses no codec |
| Zstd probes at fixed offsets | Avoid repeated failed compression attempts | Same-input wall/CPU/object/physical comparison, including probe cost |
| Original ZIP record boundaries | Stable update reuse despite entry insertion | Bounded parser, exact reconstruction, fixed/CDC/container update comparison |
| Code/metadata category | Possible startup prior | Actual access trace and startup measurements; deferred |
| Engine version / executable model | Reproducible workload description | Scanner report cross-checked against current source identity |
| Anti-cheat signals | Compatibility-validation gate | Absence does not prove safety or support |
| Store/appid/name | Human discovery metadata | No ownership proof and no compression decision |
| Loaders/routes/mod/saves/Workshop paths | Modding metadata only | Omitted; no storage benefit demonstrated |

Physical bytes can worsen when sampling misses compressible regions. Pack CPU
can improve only if saved codec attempts outweigh sampling/hash overhead.
Random reads and sequential throughput may benefit from raw objects or smaller
boundaries, but extra objects/metadata may worsen latency and amplification.
Cross-version reuse may improve only for containers whose unchanged record bytes
remain stable. Runtime startup behavior is unmeasured; no engine-seeded prefetch
or change to static LRU defaults is justified in this version.

## Selected first implementation

Add a pure Rust offline analysis crate, strict versioned GameProfile and
PackingPlan sidecars, optional bounded `um scan SOURCE --json` adapter, full
source-content identity and deterministic bounded Zstd/entropy/repetition probes.
Imported labels remain advisory; source identity, codec decisions and container
boundaries are recomputed before packing. Generic API/defaults stay unchanged.
ZIP support deliberately accepts a narrow unencrypted, single-disk, non-ZIP64
subset with consistent central/local records. Unsupported/malformed containers
use generic CDC without changing bytes. Do not extract, decode or rewrite entries.

No proprietary archive parser, dictionaries, lossy transcoding, encryption-key
discovery, runtime Python, loader/injection/debugger, DRM/anti-cheat bypass,
assets, commercial decompiled code, or network knowledge refresh is integrated.
Store v1 already accepts contiguous variable chunk references and raw/Zstd;
there is no format bump and profiles need not accompany mounted stores.

## License/dependency implications

Universal-modder is [MIT licensed](https://github.com/rehan-remade/universal-modder/blob/0f5dcdfdcd8ed420f8413815bd6647586ab894a2/LICENSE)
by Rehan and contributors. Implement a clean adapter rather than vendor its code.
Keep attribution and a copy of its notice with research provenance. Installation
is optional and manual. Existing PlaySparse/WinFsp dependency-license conditions
remain. No plugin or external tool is automatically installed.

## Measurement plan

Experiment 05 will retain all trials at L0, rotate configurations, compare the
exact same generated files/versions and queries, and report medians and variance.
Compare current-main generic CDC and fixed with measured-codec CDC and ZIP-aware
CDC. Count object/metadata/physical/allocated bytes, pack wall/CPU, userspace-cold
and warm p50/p95/p99, sequential throughput, read CPU/RSS/cache/amplification,
and v2 objects/bytes absent from v1. This is logical cross-store reuse opportunity,
not a shared-store update API. OS/device caches are uncontrolled; disk-cold,
startup, physical-device I/O and L1/L2 game compatibility stay unknown/unrun.
Keep negative outcomes and separate discovery benefit from storage-frontier gain.
