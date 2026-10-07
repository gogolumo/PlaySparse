# Downloadable physical desktop acceptance

Download **both** matching `playsparse-development-{platform}` and `desktop-validation-kit-{platform}` artifacts from the same successful desktop workflow commit. Install/extract the application using normal trusted-development procedures. Install FUSE 3/WinFsp and WebView2/WebKitGTK separately using official guidance. The kit never elevates or installs drivers. Python 3 is required by the Linux wrapper; Windows uses PowerShell.

Extract the validation kit (containing build-manifest.json, engine, fixture and validation binary). Verify its supplied checksums; these detect accidental corruption and are not publisher authentication. The helper checks embedded validation-tool commit provenance against the manifest. Use a fresh writable work path separate from any game installation. Windows:

```powershell
./validation/windows/validate.ps1 -App 'C:\Program Files\PlaySparse\PlaySparse.exe' -Work "$env:TEMP\playsparse-physical-new"
```

Linux (use the installed deb application executable, or a runnable AppImage):

```sh
bash validation/linux/validate.sh /usr/bin/PlaySparse /tmp/playsparse-physical-new
```

The actual executable name/path depends on the package; choose the installed application binary. Type `VISIBLE` only after observing the actual library window. This is a manual visibility attestation, not automated native IPC interaction. GUI uses a separate disposable library. The Rust service harness then registers a generated installation and exercises engine/readiness, analyze, optimize, verify, mount, exact read, overlay write, launcher-child tracking, blocked unmount during child activity, root stop, natural child exit, ordinary unmount, post-unmount verify and source hashes. The fixture child lasts five seconds; Stop intentionally does not terminate it. No copyrighted assets are used.

`validation-windows.json` or `validation-linux.json` and `summary.txt` include OS version/architecture, version/compiled commit, build-dirty status, provider/readiness, per-stage status/timestamps/errors and before/after source hashes. Preserve the entire work directory on failure. Close any game processes, inspect the actual filesystem and use only ordinary unmount. Stale mounts are not automatically detached. Receipts contain paths in failure evidence; review before sharing. Diagnostic exports use a stricter path-free allowlist.

A kit built/run on GitHub uses **CI simulation** for portable service tests. Native process tests are **hosted native test**. Neither is **physical hardware validation**. The physical wrapper labels the declared evidence level, which still requires the operator to identify a real machine. Never use the physical label for a VM/hosted runner. Commercial game compatibility is always NOT VERIFIED by this generated kit.

The kit does not automate the native GUI workflow beyond visible app startup. To validate full Tauri IPC interaction, use the existing macOS `tools/desktop-native-acceptance.py`; Windows/Linux GUI interaction beyond visibility remains a separate manual gate. On a clean macOS checkout the additional signed-code probe is:

```sh
python3 scripts/validation/macos/native-code.py --engine target/release/playsparse --work /tmp/playsparse-native-code-new --commit FULL_COMMIT_SHA
```

This probes minimal ad-hoc native code; it is not a Developer ID, notarization, dynamic-library or commercial-game claim.
