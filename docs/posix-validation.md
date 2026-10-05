# Native macOS and Linux validation

Run from the PlaySparse repository with Python 3.9 or newer and the Rust toolchain
specified by `rust-toolchain.toml`. `posix-runtime-validation.py` builds and runs
the same real mounted workflows used by runtime CI. It never installs packages,
drivers, changes privileges, or overwrites an existing evidence directory.

## macOS first

Before installing a driver, the ordinary binary provides a passive diagnosis:

```bash
cargo run --locked --release --bin playsparse -- doctor --human
target/release/playsparse doctor --human --mount-test
```

The first command creates no mount. The second checks prerequisites and returns
`BLOCKED` with exit code `2` when the driver or compiled backend is missing. JSON
is the default when `--human` is omitted. Diagnosis distinguishes an absent
driver, absent compiled support and an unsupported driver version.
`READY_TO_TEST` means a mount can be attempted; installation alone leaves kernel
approval `UNKNOWN`. A successful test establishes runtime availability. A
permission/device failure produces `BLOCKED`; a failed probe or cleanup produces
`FAIL`. The explicit mount test uses a tiny generated base and a new owned
temporary directory, checks exact probe bytes and the observed mount, unmounts
normally and records cleanup. It preserves that directory if cleanup fails.

Native mounts require macFUSE 5.3.3+ in the 5.x series (5.4.0 SDK verified) at
`/Library/Filesystems/macfuse.fs`. Complete any macOS driver approval and restart
required by its kernel-backend installer before testing. PlaySparse does not
yet support the separate FSKit transport. Detection of the installation alone
does not establish that the driver can mount; the tests perform actual mounts.

After completing the manual installation/approval, rebuild the native backend
and run the small test before the full workload:

```bash
cargo build --locked --release --workspace --features macfuse
target/release/playsparse doctor --human --mount-test
```

Native compilation requires a working `pkg-config` command and the installed
macFUSE headers/libraries. If compilation fails, retain its output and resolve
that prerequisite before attempting mounts. Installing the driver and approving
its kernel backend are separate manual steps described by the official
[macFUSE Getting Started guide](https://github.com/macfuse/macfuse/wiki/Getting-Started).
The runner never disables SIP or Gatekeeper or changes macOS security settings.

```bash
python3 tools/posix-runtime-validation.py \
  --work /tmp/playsparse-macos-01 --build
```

On macOS `--build` adds `--features macfuse`. A binary built without that feature
reports a blocked prerequisite in `doctor`. If the driver is absent, the runner
returns `BLOCKED` and preserves the environment and reason without building or
attempting a substitute mount in a Linux container.

## SDK checks without installing a driver

```bash
python3 tools/macos-sdk-check.py --work /tmp/playsparse-macos-sdk-01
```

This fetches the checksum-pinned official macFUSE 5.4.0 image, verifies the
Apple installer signing identity and notarization, attaches it read-only and
extracts its libraries to the new work directory. It runs fmt, clippy, tests and
release build with `macfuse` enabled, then detaches the image. No driver, system
library or security policy is installed or changed. SDK checks do not establish
that a real volume can mount. Logs and result JSON are retained; the SDK/image
and compiled binaries stay outside Git.

The SDK checker also requires `pkg-config`, a clean checkout and the pinned Rust
toolchain. `--allow-dirty` explicitly records a development checkout;
`--stage-timeout` bounds each compiler command (default 1800 seconds, valid
60..86400). Downloaded bytes are bounded and checksum checked before attachment.
SIGINT/SIGTERM and command failures retain logs and attempt ordinary detachment
of the owned image. A mount-table inspection or detach failure changes the
result to `FAIL`; successful compilation cannot hide failed image cleanup.
Repository identity must remain unchanged for the run to pass.

## Linux

The machine must expose `/dev/fuse` as a character device that the testing user
can open for reading and writing, and provide `fusermount3` (normally from
`fuse3`) or `fusermount`. An administrator controls package installation, device
access and mount privileges. The current Linux backend builds `fuser` without
its libfuse feature, so a libfuse development package is not required by this
build. Python, Git, Rust and a C linker must already be available.

```bash
python3 tools/posix-runtime-validation.py \
  --work /tmp/playsparse-linux-01 --build
```

No automatic `sudo` is used. If the machine's mount policy requires elevation,
choose the appropriate execution account before starting. In a container,
device and mount permission must be provided by its operator. Evidence includes
container/virtualization hints; a Linux Docker run establishes real Linux FUSE
behavior, not native macOS or physical Linux hardware performance.

## Existing binaries and one application

To reuse binaries, omit `--build` and provide paths if they are outside
`$CARGO_TARGET_DIR/release` or the default `target/release`:

```bash
python3 tools/posix-runtime-validation.py \
  --work /tmp/playsparse-posix-02 \
  --playsparse target/release/playsparse --io-probe target/release/io-probe
```

The canonical runner requires a clean checkout and a matching local build
receipt. `--build` writes `playsparse-build-provenance.json` next to the binary,
after checking that the repository did not change during the build. The receipt
binds Git revision, tracked/nonignored file digest and binary hashes. It is local
provenance, not a signed release attestation. `--binary-manifest` selects a receipt
when binaries were copied elsewhere; a known hash or source mismatch blocks the
run and requires rebuilding. Relative `CARGO_TARGET_DIR` values resolve against
the repository, matching Cargo's build working directory.

For deliberate development testing, `--allow-dirty` and
`--allow-unverified-binaries` explicitly record those limitations. They do not
override a known mismatch in an existing receipt. Direct smoke scripts retain
their existing interfaces. Work directories and application installations must
remain outside the repository, and must not contain each other. Paths containing
spaces work when quoted in the caller's shell.

Optionally run an application that you own after the generated tests. Supply its
unmodified installation and an executable relative to that installation:

```bash
python3 tools/posix-runtime-validation.py \
  --work "/tmp/playsparse app 01" --build \
  --source "/path/to/My Application" --executable "bin/My Application" -- --self-test
```

The helper packs a separate base, mounts it with a writable overlay, launches
the executable directly with the supplied arguments, and checks the original
installation and base afterward. Assets remain local. Applications must be
native to the host OS; this runner does not provide Windows emulation. Source
and base fingerprints are saved before and after, including failed launches.
The helper records child exit status and keeps stdout/stderr. It does not sandbox
arbitrary application access outside the mount; any detected source/base change
fails validation rather than repairing or deleting the changed data.

The default classification is application evidence: real game validation remains
`NOT RUN` and `GAME EVIDENCE REQUIRED`. For a legally owned game, add
`--application-kind game` to explicitly classify the supplied installation.
Generated TestGame fixtures remain synthetic even with that option. A successful
game executable invocation covers those arguments only; Steam/Epic or other
launcher compatibility remains a separate gate. No executable patching, DRM or
anti-cheat changes are performed.

## Evidence and interpretation

Each new work directory contains `environment.json`, `result.json`, command
stdout/stderr logs, and the complete stage evidence:

- `readonly`: generated 10 GiB sparse fixture, offsets above 4 GiB, mmap,
  mounted executable and original-versus-mounted I/O checks.
- `writable`: updater, remount, commit to a new verified base, discard a copied
  overlay, full tree comparison and source/base immutability checks.
- `adaptive`: captured mounted traffic, identical static/adaptive replay in
  separate processes, cache and prefetch measurements. Correctness `PASS` does
  not mean adaptive policy improves performance.
- `tiers`: secondary promotion, offline remount, loopback HTTP object ranges
  and rejection of corrupt remote bytes.

The top-level report records binary SHA-256 hashes, the Git revision and dirty
status, local build provenance, exact argv arrays, per-command exit status and
timings, mount backend, evidence type and incomplete stages as `NOT RUN`.
Application evidence includes source hashes and base immutability. Atomic JSON
replacement preserves the previous complete report if publication fails.
Kernel/device caches are uncontrolled; measurements are not disk-cold results.
No physical-machine claim follows from the absence of virtualization hints.

Before generated workloads, free scratch space must meet `--min-free-bytes`
(default 512 MiB). This is a minimum, not a bound on all build/application disk
usage. Application runs additionally require enough space for two full logical
copies, for the encoded base and possible whole-file copy-up; application writes
may require more. Insufficient space returns `BLOCKED` before mounting. The
generated 10 GiB fixture is sparse, not a reserved 10 GiB allocation.

Exit codes are `0` for all requested stages passing, `1` for execution failure,
and `2` for a missing prerequisite. Failed logs remain in place; choose a new
`--work` directory for the next attempt. The default timeout is 1800 seconds per
stage; `--stage-timeout` accepts 60 through 86400 seconds, including an optional
interactive application. SIGINT, SIGTERM and timeout first allow subprocess
cleanup, then terminate the owned process group (Windows helper uses taskkill's
process tree while its direct child is alive) and reap the direct child. The runner attempts ordinary unmount of
its surviving mountpoints and checks they detach; it never force/lazy-unmounts a
foreign path. Processes that deliberately escape a Unix session are outside
this process-group guarantee. On Windows the helper does not yet place children
in native Job Objects, so descendants surviving an already-exited direct child
are outside its tree-cleanup guarantee. Failed launch/unmount/identity checks produce
`FAIL`, with partial evidence retained. After interruption, inspect the recorded
mountpoint before reusing resources. Reused work directories are refused without
modifying their evidence.
