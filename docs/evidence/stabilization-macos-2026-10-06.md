# Phase 0/1 stabilization — native macOS acceptance

## RESULT

**PASS for repository consistency and the implemented validator, workspace,
doctor and trace fixes.** This does not complete the desktop product mission.

Tested code revision: `a57293cd56cf7d2be175afd006775471e330eff1`.
The final native run used a clean checkout and verified local build receipt;
repository and binary identities were unchanged throughout the run. An earlier
dirty-checkout run also passed and was used for development, not the final receipt.

## COMMITS

| Commit | Change |
|---|---|
| `fb017ea` | Synchronize roadmap and audit product gates against physical evidence |
| `9cd71f4` | Classify AppleDouble metadata during mounted validation |
| `d7188b5` | Temporary workspace control and disk-budget/doctor diagnostics |
| `a57293c` | Exact bounded external read-range aggregation |
| `f145144` (separate branch) | PR #13 lossless curated evidence replacement, [draft PR #14](https://github.com/gogolumo/PlaySparse/pull/14) |

## FILES CHANGED

- `ROADMAP.md`, `README.md`, `docs/productization-audit-2026-10-06.md`
- `docs/limitations.md`, `docs/temporary-storage.md`, `docs/access-tracing.md`
- `tools/metadata_validation.py`, `tools/mounted-update.py`, `tools/tiered-smoke.py`
- `tests/test_metadata_validation.py`
- `crates/playsparse-cli/src/{main,game,diagnostics,workspace}.rs`
- `crates/playsparse-vfs-win/src/backend.rs`
- `crates/playsparse-trace/Cargo.toml`, `crates/playsparse-trace/src/{lib,ranges}.rs`
- This report and `bundles/stabilization-macos-20261006.{tar.gz,inventory.json}`

The separate evidence branch preserves PR #13's report, a full raw bundle with
2,581 member hashes, restoration instructions and curated results/provenance.
PR #13 itself remains unchanged and open.

## TESTS

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo test --locked --workspace --all-features` | 121 passed, 2 driver-backed ignored tests |
| `cargo build --locked --release --workspace --all-features` | PASS |
| `PLAYSPARSE_GAME_TEST_BINARY=target/release/playsparse python3 -m unittest discover -s tests -p 'test_*.py'` | 54 tests, 2 platform-specific skips, no failures |
| `python3 tools/posix-runtime-validation.py --work /tmp/playsparse-productization-final-20261006 --build` | PASS from clean tested revision |
| Doctor native mount / exact bytes / different device / ordinary unmount | PASS |
| Readonly generated mounted suite | PASS |
| Writable update / remount / commit / discard | PASS |
| Adaptive generated mount/replay | PASS mechanics; no performance-win claim |
| Secondary-local promotion / offline promoted remount / HTTP ranges / corruption | PASS |

Writable tree comparisons classified eight sidecars each at update, remount and
commit; discard had none. Tiered offline remount classified one sidecar. Raw
inventories and hashes were preserved; no expected application entry was excluded.
Source and immutable base fingerprints stayed identical in writable and tiered
runs; tiered secondary fingerprints also stayed identical. The validator does
not claim xattr/resource-fork preservation by the storage format.

Regression coverage includes ordinary `._*` content mutations/deletions, orphan
and malformed metadata, data-fork envelopes, truncation/overlap, content corruption
with valid sidecars, unchanged strict Linux/Windows comparison and explicit xattrs.
Trace tests merge runs across 110,000 ranges and verify exact rereads/hottest range;
existing hostile-input/event/worker/string limits remain enforced.

Additional real CLI checks passed: chosen temp location with spaces, cleanup,
source byte invariance, nested-source workspace rejection, and early analyze/pack
rejection for a generated sparse 1 TiB input before creating output directories.
The error named `/System/Volumes/Data`, required/available byte counts and the
`--temp-dir` remedy. This simulates a budget shortage; it is not a physical disk-full
or Windows error-112 test.

## PLATFORMS ACTUALLY TESTED

Native macOS arm64, installed macFUSE 5.4.0, real kernel-backed mounts, generated
L0 data. No new commercial game or new storage-saving measurement was run.
**Windows/Linux: NOT PHYSICALLY TESTED by this implementation phase.** Hosted
checks remain separate and are visible on the implementation PR.

## EVIDENCE

The [raw bundle](bundles/stabilization-macos-20261006.tar.gz) retains 197 files:
commands, build receipts, binary hashes, doctor/environment results, stage results,
tree comparisons, logs and traces. No source fixture files, binaries or game assets
are included. [Inventory](bundles/stabilization-macos-20261006.inventory.json)
records per-member path/length/SHA-256.

Bundle bytes: **249,528**. SHA-256:
`6bdd513813c6de593359981090f09cf90018e5f130a5abbad2cf58b77c8a4c55`.
Every member was verified after reopening the final archive. Extract to a new
evidence directory (members are relative to the final run root) and inspect
`result.json`, `binary-build-manifest.json`, `writable/evidence/*-tree.json` and
`tiers/evidence/*-tree.json`. The canonical report confirms repository/binary
invariance, real mounts, stage results and original commands.

## KNOWN LIMITATIONS

- Budgets are conservative estimates, not reservations; concurrent disk usage,
  quotas or unusual allocation can still cause ENOSPC. Overlay copy-up/commit and
  promotion-cache capacity remain follow-up work.
- Pack has no `--temp-dir`: atomic staging is on the selected STORE volume.
- Trace has exact range metrics but still limits 2M events, 100k file/session/stream
  identities and 32 MiB retained string keys. External sorting uses default OS
  TEMP and at most 48 MB raw scratch plus filesystem overhead. Kills can leave scratch.
- Doctor's mount probe uses default OS TEMP; custom `--temp-dir` reports that
  location. Windows doctor probe remains delegated to the separate native harness.
- Metadata is classified and recorded, not claimed preserved by immutable v1.
- General session/recovery/launch orchestration, native-code cache, case-compatible
  shadow, GUI and packaging remain future work. Existing two single-game reports
  remain title/host-specific; no universal compatibility claim is added.

## NEXT HIGHEST-PRIORITY TASK

Phase 2: structured per-file/extension storage composition from measured stores,
then bounded entropy and isolated read-only container analysis. Keep similarity/
delta experimental and static LRU/default codecs unchanged unless measurements
justify a change. Implement shared sessions before desktop work.
