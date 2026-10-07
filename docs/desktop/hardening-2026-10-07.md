# Desktop production hardening — 2026-10-07

PR #18 is merged. PR #19 was OPEN when work started and was neither merged nor modified. This branch starts from updated `main`, not PR #19's branch. Existing stores, originals and overlays are retained.

## Architecture

A launch root is the owned current-session `Child`; a launcher is a root that creates a game child. A descendant is a process observed in the isolated Unix launch group or via an identity-verified parent edge, or a member of the Windows Job handle. A game is not selected by filename: all observed live members keep the session active. Launching, Running, LauncherExitedButGameRunning, Exited and NeedsAttention are separate lifecycle observations.

Windows uses CREATE_SUSPENDED, AssignProcessToJobObject, Toolhelp thread enumeration and ResumeThread. Assignment/resume failure terminates only the owned suspended root. Jobs do not enable breakaway or kill-on-close. Numeric PIDs are display data only; Job handles establish membership. External broker/WMI launches are outside that containment guarantee. Stop terminates only the owned root, retaining descendants; it does not kill arbitrary members by a stored PID. If PlaySparse crashes between process creation and job assignment, an inert suspended root may remain for manual inspection. This narrow crash window needs a future atomic PROC_THREAD_ATTRIBUTE_JOB_LIST launch implementation; it is not claimed solved.

Linux uses a new process group and `/proc/<pid>/stat` start-time identities. macOS uses libproc BSD start timestamps, groups and parent edges for the current UID. Matching identities preserve observed descendants after reparenting or detachment. Root PID reuse disables group adoption; no numeric group or descendant PID is ever signalled. macOS `.app` continues to use Launch Services; its helper does not own the actual app, which must be quit normally. Snapshot errors fail closed. The GUI's existing 750 ms reconciliation interval avoids millisecond scanning.

Unix setsid/double-fork children can escape between samples. Unix `safe_to_unmount` therefore remains false even when observed processes have exited. Ordinary unmount requires explicit user confirmation that all game/launcher processes are closed, successful current process inspection and live mount ownership. External/bundle launches without identity proof require external inspection/detachment followed by absence-only recovery. No force-unmount, persisted-PID killing or automatic crash cleanup is provided.

Mounts get an random tempfile-derived token written/read through the actual writable mount. Ordinary unmount requires the token, the expected application-owned location and a live owned backend Child in the current app lifetime. After restart, path/token alone cannot authorize detachment: the user must inspect/detach externally and then Recover after absence. The token is not an access-control boundary against a malicious process running as the same user. Old identity marker files remain in the overlay; bounded marker cleanup is follow-up work.

Launch confirmation records a bounded BLAKE3 executable fingerprint. Source and mounted executable bytes must still match immediately before launch. Traversal, symlink components, external hardlinked Unix executables and oversized inputs are rejected. Literal argv remains the only game invocation path. Fingerprint checking reduces accidental/existing replacement but does not eliminate concurrent replacement in a writable mount between hashing and exec. OS executable handle binding/read-only launch overlays need further design; adversarial same-user races are NOT VERIFIED.

## Recovery and durability

Before publishing `library.json`, the service writes/fsyncs an atomic `library.backup.json` of its previous parseable state. New metadata uses an exclusive tempfile, fsync and atomic rename; Unix also fsyncs the directory. Corrupt input is never reset or implicitly restored. `playsparse-validation --restore-library /path/to/app-data` validates a bounded backup, retains corrupt JSON in an exclusive evidence file, atomically restores metadata and requires reopening to reconcile sessions. Close the app first. This restores metadata, not immutable store objects or saved games. Keep independent backups; a single rotating previous-state backup is not a historical backup service. Windows power-loss durability of rename/directory metadata is NOT VERIFIED.

Rebuild Store is offered for a failed/missing store with no session. It verifies corruption, preflights space, creates a unique sibling store using engine staging/publication, verifies it, then atomically publishes a new library pointer. The old store never moves or disappears. Failure/cancellation leaves the old pointer, source and corruption evidence intact. A crash after new store publication but before pointer publication leaves a recoverable orphan. Existing overlays stay on disk; the user must inspect save compatibility with the rebuilt current-source version. Repair is asynchronous with real engine progress. It never patches corrupt objects in place.

Library entry counts, game identities, launch argv, compatibility text and JSON sizes are bounded. Store verification never implies source-version equality; after source updates, rebuild uses current source explicitly. Recovery tests cover truncated metadata, retained unpublished temporary writes, backup restoration, corruption, failed rebuild, stale sessions and unchanged originals. They simulate interruptions, not physical power failures.

## Compatibility, UI and diagnostics

See [compatibility profiles](../../compat/README.md). Unknown, Detected, Tested, Compatible, CompatibleWithLimitations, Unsupported and Broken have explicit provenance requirements. Synthetic profiles cover plain, launcher-child, multiple, case-sensitive, writable-save, missing and crashing workloads. Discovery creates Detected; launch confirmation never creates Compatible. Import/certification UI and commercial profiles are not provided. Records are evidence data, not signed certifications.

Backend snapshots supply available runtime actions. The UI shows compatibility, last store verification, active process count, launcher-exited running state, rebuild and Export diagnostics. Invocation revalidates every action. The diagnostic JSON allowlists version, OS/arch, provider/readiness state, storage measurements, job stages/state/timestamps and error presence. Names, all paths, argv, raw errors/logs, PIDs, session tokens and arbitrary file contents are omitted. This is stricter than home-directory replacement. Export does not silently collect runtime logs.

## Validation and evidence boundaries

| Feature | macOS | Windows | Linux |
|---|---|---|---|
| Build | Local Rust/Tauri checks; hosted packaging gate | Hosted NSIS gate | Hosted deb/AppImage gate |
| Physical desktop test of this sprint | Generated service mount/read/write/root stop tested; full GUI acceptance separate | NOT VERIFIED | NOT VERIFIED |
| Mount | Native macFUSE generated fixture tested | Physical kit prepared; NOT VERIFIED in this sprint | Physical kit prepared; NOT VERIFIED |
| Launch | Generated shell and minimal ad-hoc native fixture tested | Job native CI gate; physical NOT VERIFIED | Native CI process gate; physical NOT VERIFIED |
| Process tracking | libproc/group synthetic tests | Job Object tests in native CI | /proc/group tests in native CI |
| Signed release | NOT VERIFIED | NOT VERIFIED | Package checksums prepared; package signing not configured |

Earlier engine/game evidence remains title/host/version specific. It does not establish this sprint's physical Windows/Linux desktop acceptance. The downloadable kit requires no Rust build toolchain. [Validation instructions](validation.md) explain receipts and manual GUI visibility confirmation. Hosted portable receipts mark excluded GUI/mount stages NOT RUN. A FAIL receipt retains its working directory/session for inspection; no forced cleanup is hidden.

The controlled macOS probe compiles a tiny C program, signs it ad hoc, verifies its signature on APFS and through macFUSE, compares exit/output and hashes source before/after. Minimal native execution passed on the available Mac. It does not reproduce the earlier Project Zomboid runtime-loaded-library problem. No APFS shadow is introduced: Developer ID signing, dlopen-heavy native code, bundles and commercial workloads need separate measured fixtures before an architecture change.

See [release preparation](release.md) for secrets, fail-closed signing and disabled updater design. All absent hardware, credentials and commercial workloads remain NOT VERIFIED.
