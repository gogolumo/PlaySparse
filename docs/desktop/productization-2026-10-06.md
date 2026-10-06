# Desktop productization MVP — 2026-10-06

Branch: `feat/desktop-productization-mvp`, from current main after merged PRs #16/#17. Existing engine/storage format and the headless macOS hdiutil packaging workflow are preserved.

Implemented: first-run system readiness and passive/actual diagnostics; bounded read-only folder inspection before Add; conservative platform launch-candidate discovery; explicit confirmation and literal argument arrays; measured analysis stage labels; destination capacity review before Optimize; separated source/store/overlay metrics and paths; file-manager inspection; backend runtime transition validation; missing-store metadata recovery; focused settings and meaningful library-corruption guidance.

Architecture: discovery and state validation live in `crates/playsparse-desktop`, not React. Tauri exposes typed inspection/preflight/readiness/recovery commands. React composes the existing service snapshot and events. Analyze/pack/verify still call the existing Rust engine directly. macOS bundles use Launch Services `open -W -n -a`; direct files use owned `Child` handles. No discovery candidate is automatically executed, and no shell interpolation is used.

## Validation

Clean native code commit `c65d176b308f3eb437ed2ecf80be0b3eafdf8b62` passed real WKWebView/Tauri IPC acceptance: installation inspection and executable discovery, passive diagnostics, actual tiny doctor mount/read/unmount (Ready and cleanup PASS), register, analyze, destination preflight, pack, verify, mount, full exact-byte read, isolated overlay write, launch, stop, unmount and another verify. Original file hashes and modes remained unchanged; the receipt reports a clean working tree. [Native receipt](evidence/productization-native-20261006.json).

- Rust workspace: 134 passed, zero failed, three driver/platform tests ignored in the normal run. Six new product tests cover inspection/path safety, duplicate sources, discovery exclusions/symlinks, bundle Info.plist validation, readiness classification, illegal transitions and missing-store recovery. Existing interrupted-session/corrupt-library/cancellation/source-invariance tests were retained.
- Formatting and strict workspace Clippy passed. Separate Tauri default-feature and all-feature Clippy passed.
- Existing Python suite: 54 tests, two platform skips, no failures.
- Frontend: clean npm installation, audit (zero vulnerabilities), ESLint, TypeScript, five Vitest tests, production build and four Playwright tests passed. Chromium installation was checked. Preview interactions cover readiness, inspection, optimization budget, candidate confirmation, existing navigation/theme/layout and error behavior.
- Native macOS Apple Silicon with installed/approved macFUSE was tested; no commercial game assets were used. Physical Windows/Linux desktop GUI validation is not established by these tests.

The native screenshot capture was **blocked** (`could not create image from window`), recorded separately from successful command/byte acceptance. No old screenshot is claimed as a current native rendering capture. Refreshed [light library](screenshots/library-light.png), [dark library](screenshots/library-dark.png), [Add Game](screenshots/add-game.png), [analysis/details](screenshots/analysis-results.png), [activity](screenshots/optimization-progress.png), [empty library](screenshots/empty-library.png) and [readiness/settings](screenshots/settings-diagnostics.png) are actual rendered **simulated Preview Mode** screenshots.

## Artifacts and CI

The initial implementation commit `2c0af0e` passed desktop/runtime/research workflows. Final functional code validation is tracked by [desktop CI](https://github.com/gogolumo/PlaySparse/actions/runs/37490288425), [runtime CI](https://github.com/gogolumo/PlaySparse/actions/runs/37490288132), and [research CI](https://github.com/gogolumo/PlaySparse/actions/runs/37490288190). Latest PR-head checks are authoritative for subsequent documentation/theme-only changes.

Local release `.app`, `.app` tarball and headlessly created/verified `.dmg` were produced from `c65d176`. [Hashes and sizes](evidence/productization-macos-artifacts-20261006.json). Hosted CI produces macOS app tarball/DMG, Windows NSIS executable and Linux deb/AppImage. These remain unsigned development artifacts, not signed/notarized production releases. The original Finder-based Tauri DMG helper was not reintroduced.

## Remaining gates

Process-tree monitoring/termination is not implemented. Only owned current-session child handles are tracked; no PID is persisted or killed after restart. Detached descendants can outlive launchers and must be closed before ordinary unmount. Bundle apps require their own Quit command; killing the Launch Services helper would not safely stop the game. No broad process-group killing is attempted.

Per-title compatibility, signed-native APFS shadows and case translation, stronger stale-drive identity proofs, signed production distribution/updates, automatic corrupt-library repair/backups and physical Windows/Linux desktop acceptance remain open. Driver presence alone is not readiness proof, and executable discovery is not compatibility evidence. Allocation unknowns remain unknown; originals stay installed, so representation savings are not disk space reclaimed. Folder-picker interaction is implemented but not automated in the native harness.

See [the desktop guide](README.md) for installation, first run, GUI workflow, recovery and the platform support matrix.
