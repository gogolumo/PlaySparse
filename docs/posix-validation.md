# Native macOS and Linux validation

Run from the PlaySparse repository with Python 3.9 or newer and the Rust toolchain
specified by `rust-toolchain.toml`. `posix-runtime-validation.py` builds and runs
the same real mounted workflows used by runtime CI. It never installs packages,
drivers, changes privileges, or overwrites an existing evidence directory.

## macOS first

Native mounts require macFUSE 5.3.3+ in the 5.x series (5.4.0 SDK verified) at
`/Library/Filesystems/macfuse.fs`. Complete any macOS driver approval and restart
required by its kernel-backend installer before testing. PlaySparse does not
yet support the separate FSKit transport. Detection of the installation alone
does not establish that the driver can mount; the tests perform actual mounts.

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

## Red Hat Linux

The machine must expose `/dev/fuse` as a character device that the testing user
can open for reading and writing, and provide `fusermount3` (normally from
`fuse3`) or `fusermount`. An administrator controls package installation, device
access and mount privileges. The current Linux backend builds `fuser` without
its libfuse feature, so a libfuse development package is not required by this
build. Python, Git, Rust and a C linker must already be available.

```bash
python3 tools/posix-runtime-validation.py \
  --work /tmp/playsparse-rhel-01 --build
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

Optionally run an application that you own after the generated tests. Supply its
unmodified installation and an executable relative to that installation:

```bash
python3 tools/posix-runtime-validation.py \
  --work /tmp/playsparse-app-01 --build \
  --source /path/to/installation --executable bin/application -- --self-test
```

The helper packs a separate base, mounts it with a writable overlay, launches
the executable directly with the supplied arguments, and checks the original
installation and base afterward. Assets remain local. Applications must be
native to the host OS; this runner does not provide Windows emulation. Without
an installation, real game validation is `NOT RUN`; a successful execution
covers that application and those arguments only.

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
status. Prebuilt binaries are not assumed to correspond to that revision.
Kernel/device caches are uncontrolled; measurements are not disk-cold results.
No physical-machine claim follows from the absence of virtualization hints.

Exit codes are `0` for all requested stages passing, `1` for execution failure,
and `2` for a missing prerequisite. Failed logs remain in place; choose a new
`--work` directory for the next attempt. The default timeout is 1800 seconds per
stage; `--stage-timeout` accepts 60 through 86400 seconds, including an optional
interactive application. Cleanup first allows existing harnesses to unmount;
after an interruption, inspect the stage's mountpoint before reusing resources.
