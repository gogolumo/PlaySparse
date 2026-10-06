# Productization intake and first stabilization phase

## Baseline and scope

Fetched origin, switched main and pulled fast-forward from a clean checkout.
Baseline: `f108e02aba6970e54cae78106dd4f2a38a73d2bb`. Implementation branch:
`fix/repository-and-validation`; evidence replacement is isolated on
`docs/curated-game-awareness-evidence`. This is Phase 0/1 stabilization, not
completion of desktop acceptance criteria.

No open issues were returned at intake. PR #13 was the only open PR; #12
(game-aware storage), #11 (native Mac evidence), #10 (macFUSE runtime) and the
earlier runtime PRs were already merged. Main's [research CI](https://github.com/gogolumo/PlaySparse/actions/runs/37334601814)
and [runtime CI](https://github.com/gogolumo/PlaySparse/actions/runs/37334601825)
passed. Hosted Mac CI validates SDK build/link/tests, not kernel-backed mounts.
Windows CI installs pinned WinFsp with checksum verification and retains hosted
status separately from physical validation. Linux CI performs genuine mounts.

Audit inputs included README, ROADMAP, CONTRIBUTING, SECURITY, limitations,
breakthrough criteria, architecture v2, storage v1, game-awareness contracts and
audit, retained Windows/Elden Ring and Mac/Project Zomboid evidence, experiment
01–05 implementations/results, Python validation helpers/tests, workspace
crate manifests/interfaces and GitHub workflows/history. Documentation was
checked against implementation entry points and evidence, not treated as proof
of untested compatibility.

## Concrete inconsistencies

1. ROADMAP still contained unchecked physical Windows/macFUSE gates and stated
   that native Windows/WOF results remained future work. Retained later reports
   establish physical generated validation, one Elden Ring executable/main-menu
   path, a warm original/WOF/PlaySparse comparison, and native Mac hybrid
   Project Zomboid gameplay. These do not establish launcher/DRM/EAC compatibility
   or broad replication. Historical reports remain unchanged; ROADMAP now
   distinguishes scopes explicitly.
2. `analyze` created two full stores in default TEMP with no capacity preflight.
   Profile-guided analysis created another temporary candidate without location
   control. Windows evidence identified system TEMP exhaustion despite ample
   space on another volume. CLI now accepts `analyze --temp-dir`; generic and
   profile analysis honor it, and pack checks its atomic destination volume.
3. Mounted-update and tier validation compared all entries as application content.
   Native Mac generated AppleDouble sidecars made intended bytes fail tree
   equality. New metadata-aware checks retain raw trees and xattrs, classify only
   new macOS metadata envelopes with expected companions, and keep strict source/
   base invariance. Existing `._*` content, corruption, missing content, unknown
   extras and Linux/Windows behavior remain strict. See Apple's
   [kernel metadata envelope definitions](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/vfs/vfs_xattr.c).
4. Trace summaries stored exact ranges in a 100,000-identity RAM map. The retained
   Project Zomboid development trace had 141,154 ranges. Exact fixed-width external
   sort now removes that range-specific limit while preserving explicit event,
   file/session/stream and retained-string bounds. Public summary regression
   covers 110,000 distinct ranges and exact reread counts.
5. Doctor's JSON had disk information for a generic path, but human output omitted
   it and there was no explicit temporary-workspace report. Both output forms now
   include volume/path/free bytes, version and compatibility limitations; explicit
   `--json` is supported. This remains a diagnostics increment, not a finished
   cross-platform dependency/install/recovery service.

## Architecture inventory

| Component | Actual current responsibilities and boundary |
|---|---|
| `playsparse-core` | Types, safe paths, BLAKE3 identity, manifest/range validation, resource measurement and anchored directories |
| `playsparse-store` | Streaming CDC/fixed packing, raw/Zstd choice, sorted pack indexes, seal validation, verify-before-exclusive-publish; verified local/HTTP tiers |
| `playsparse-range` | Intersecting-chunk reads, bounded buffers/cache, shared loads, replay and opt-in prefetch |
| `playsparse-cache` | Byte-bounded LRU and optional decaying-priority eviction; no default adaptive superiority claim |
| `playsparse-overlay` | Whole-file copy-up, retained handles, tombstones, atomic digest-checked namespace snapshots; no multi-file power-loss transaction |
| `playsparse-trace` | Bounded asynchronous JSONL writer, strict event import, exact summary; disk-backed range aggregation in this phase |
| `playsparse-policy` | Strict versioned opt-in policy, bounded observations and trace-derived priorities |
| `playsparse-game` | Offline identity/probes/scanner adapter and restricted original-byte ZIP boundaries; no BDT extractor or delta codec |
| `playsparse-vfs-fuse` | Linux FUSE and macFUSE callbacks; separate readonly/writable namespaces; POSIX case-sensitive behavior |
| `playsparse-vfs-win` | WinFsp direct-buffer callbacks, Windows namespace/collision rules, attributes, overlay and mount teardown; native volume reporting added |
| `playsparse-cli` | Engine commands and mount orchestration; no shared application session state machine or general launcher yet |
| `io-probe` | Independent identical-byte original/mounted I/O checks and resource measurements |

The Python lab is research code, not the runtime data plane. Experiment 05's
ZIP result is an **update-efficiency tradeoff**: better hypothetical cross-version
CAS reuse can cost full-store size and packing CPU. Sampling skip-compression
keeps its adverse-input negative result and explicit opt-in. No default codec,
cache policy, storage format, signing policy or source-deletion behavior changed.

## PR #13 retention

PR #13 head `84faf91e6c2b183fdd2d26d316d46af95407207f` adds 2,584 files and
271,421 lines. The raw tree contains 2,581 files, 18,690,952 bytes: 886 JSON,
1,642 logs, 51 JSONL traces, one text inventory and one Python analysis helper.
Expanded duplicate CI logs do not need permanent separate Git entries.

[Draft replacement #14](https://github.com/gogolumo/PlaySparse/pull/14) retains
all raw bytes in a deterministic 1,479,836-byte gzip/tar bundle plus per-member
paths, lengths and SHA-256 hashes. It also keeps important expanded reports,
results, provenance, inventories and native failure stderr. Reopened bundle
verification checked all 2,581 original hashes. PR #13 was neither modified nor
closed. Bundle retention in Git deliberately avoids dependence on expiring
Actions artifacts; future releases may move raw bundles to durable release
assets with the same verification contract.

## Initial validation result

**RESULT: PASS for the implemented stabilization scope.** Local all-feature
workspace tests and strict all-target Clippy pass. Python metadata/lifecycle
regressions pass. Native Apple Silicon/macFUSE generated readonly, writable,
adaptive and tiered suite passes; doctor mounts, reads exact bytes and unmounts.
Writable source/base fingerprints and tiered source/base/secondary fingerprints
remain unchanged; update/remount/commit/discard content checks pass while
AppleDouble sidecars are explicitly recorded. See the separate phase evidence
report for final revision, commands, hashes and exact counts.

Real CLI checks exercised a chosen temp path containing spaces, cleanup, source
byte invariance, source-contained workspace refusal, and early analyze/pack
rejection of a generated sparse 1 TiB file without creating output directories.
The latter validates deterministic budgeting, not physical ENOSPC behavior.

**Platforms actually tested:** native macOS arm64, installed macFUSE kernel
backend, generated L0 workload. **Windows/Linux changes: NOT PHYSICALLY TESTED**
in this implementation phase. Hosted checks are reported independently. No new
commercial-game test or new compression benchmark was performed.

## Remaining product gates / next phase

Structured per-file/extension analysis is next. Existing inspect probes already
provide bounded entropy samples but have an 8,192-entry limit; generic measured
analysis should expose store-based composition without requiring that profile.
Container hints need isolated read-only adapters and explicit unknown results.
Similarity/delta remains experimental research with no default format change.

Disk preflight is an estimate, not a reservation; quota/concurrent allocation can
still fail after it. Overlay copy-up/commit and promotion growth need additional
capacity controls. Trace remains bounded at 2M events and 100k file/session/stream
identities; killed analysis may leave scratch. Xattr/resource-fork preservation
is not implemented by v1. Doctor's mount probe still uses default OS TEMP;
Windows doctor mount-test still requires the separate Windows harness.

Shared sessions, general launch descriptors, recovery CLI, verified macOS native
cache/case mode/automatic shadow, desktop shell ADR/GUI and installable release
packaging remain future implementation. Project Zomboid's measured 42.75% is a
single manual hybrid result; Elden Ring's ~3.2% is a single generic result. Neither
is a universal saving or compatibility claim. No Phase 5 UI is built on these
remaining Phase 3/4 gaps.
