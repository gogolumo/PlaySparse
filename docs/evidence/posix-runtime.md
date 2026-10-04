# macOS-first POSIX runtime continuation

Starting revision: `05e23e7` (merged adaptive writable sprint). The user chose
native Mac execution first, with Linux as another target. No Windows
hardware is available. This continuation preserves the immutable CAS and common
overlay/resolver; it changes only platform mounting, path normalization and
validation tooling.

## macFUSE compatibility fix

The pinned fuser 0.18 macOS helper calls `fuse_mount_compat25`. macFUSE 5.3
disabled that entrypoint. The supported transport now calls public `fuse_mount`,
retains the returned owning channel, duplicates its borrowed `fuse_chan_fd` with
close-on-exec, and feeds fuser's existing `Session::from_fd`. The channel outlives
the session and is released once. Ordinary OS unmount avoids the ambiguous
version-dependent ownership of `fuse_unmount`. Runtime accepts macFUSE 5.3.3+
in the 5.x series; older transition releases and FSKit are rejected.

Primary references: [upstream issue](https://github.com/cberner/fuser/issues/752),
[public descriptor implementation](https://github.com/macfuse/library/blob/5c08befc7090bbe7f7c666dc2941aaba9e8537de/lib/fuse_session.c#L260),
[channel reference counting](https://github.com/macfuse/library/blob/5c08befc7090bbe7f7c666dc2941aaba9e8537de/lib/fuse_session.c#L217)
and [official 5.4 release](https://macfuse.github.io/2026/09/07/macfuse-5.4.0.html).
FSKit requires a message-channel transport; it is not established by adding a
mount option to a device-fd reader. No native mount PASS is claimed by this fix.

Mac-enabled tests also exposed noncanonical system aliases in FUSE fixtures.
Private fixture roots now use canonical paths. Overlay containment checks
canonicalize store/mount paths too, so `/var` and `/private/var` cannot produce
different overlap results. Physical overlay storage still rejects symlinks.

## Native Mac compiler/library proof — PASS

[SDK result](raw/posix-runtime-20261004/macos-sdk/result.json), executed from
clean `2a9ee0e`, records native macOS 26.6.2 arm64, exact source-file hashes and
every command. The official macFUSE 5.4.0 DMG matched SHA-256
`861814f0ac7fa8f6547ea40cdd49a36ac84bcc7d34f38a1fa74e8cf68b0401c5`.
`pkgutil` verified the Benjamin Fleischer/3T5GSNBU6W installer identity and
Apple notarization. The image was attached read-only and extracted to temporary
storage; its libraries were used through temporary search paths. No driver or
system library was installed, and the image was detached normally.

Fmt, clippy with `-D warnings`, all 76 macFUSE-enabled workspace tests and release
build passed. Two actual-mount tests remained explicitly ignored. The three new
Mac tests cover borrowed-fd duplication/close-on-exec ownership, option injection
and unsupported transport rejection, and the supported-version gate. Neither
these unit tests nor compilation prove that a volume mounts.

## Physical Mac mount — BLOCKED

[Preflight](raw/posix-runtime-20261004/macos-preflight/result.json) records that
`/Library/Filesystems/macfuse.fs` is absent. The runner returns exit 2/BLOCKED
before attempting a build or mount. The installer and kernel extension approval
require user interaction; no permission, driver or startup-security policy was
changed. Follow the [official kernel-backend instructions](https://github.com/macfuse/macfuse/wiki/Getting-Started)
and then run the [POSIX harness](../posix-validation.md). FSKit compatibility
and real Mac game/application execution remain NOT RUN.

## Real Linux runtime proof — PASS

The [fresh runner result](raw/posix-runtime-20261004/linux/result.json) and
[environment](raw/posix-runtime-20261004/linux/environment.json) record clean
`2a9ee0e`, a locked release build by the runner and both binary hashes. This is
Debian userspace in the existing privileged LinuxKit/aarch64 VM, with real
`/dev/fuse`; it is not native Mac evidence or a physical Linux hardware measurement.
All four stages pass: generated 10 GiB readonly/mmap/native executable, writable
updater/remount/immutable commit/copied-overlay discard, identical-trace
adaptive replay, and verified promotion/offline remount/exact HTTP/corruption.
Each stage retains its full result, traces and logs under the same evidence root.
Correctness PASS does not override the existing negative adaptive measurements.

All nine FUSE crate tests, including both real mounted tests, also passed locally
on Linux during transport integration. The new macOS SDK CI job never installs
a driver and never represents its result as mounted macOS proof. Native Linux
and WinFsp CI remain regression checks; physical Windows stays untested.

## Reproduction

```bash
python3 tools/macos-sdk-check.py --work /tmp/playsparse-macos-sdk-next
python3 tools/posix-runtime-validation.py --work /tmp/playsparse-native-next --build
```

Both work paths must be new. The second command runs on native Mac only after
macFUSE is installed/approved, or on Linux with accessible `/dev/fuse` and
`fusermount3`. The [guide](../posix-validation.md) covers Linux prerequisites
and the optional local owned-application test. No game assets, temporary stores,
SDK binaries or installer images are committed.
