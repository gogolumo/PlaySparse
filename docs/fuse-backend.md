# FUSE development backend

The Rust backend mounts an immutable PlaySparse store as a real, read-only
filesystem. A kernel read calls `RangeResolver::read_range`; that resolver loads
and verifies the required CAS chunks. The backend never extracts files or creates
a reconstructed directory. Windows remains the production gaming target.

## Linux

Install the FUSE device/helper and a C compiler for the native executable test:

```sh
sudo apt-get install fuse3 build-essential
cargo build --release -p playsparse-cli
mkdir /tmp/playsparse-mount
target/release/playsparse mount /path/Game.playsparse /tmp/playsparse-mount --cache 512M
```

The mount command stays in the foreground. In another terminal, programs can
open files, enumerate directories, map files and execute binaries through the
mounted path. Finish with:

```sh
target/release/playsparse unmount /tmp/playsparse-mount
```

The mountpoint must exist and be empty, and the store and mountpoint must be in
separate directory trees. Ordinary user mounts need access to `/dev/fuse` and an
installed `fusermount3` or `fusermount` helper. Container mounting also requires
the FUSE device and mount permission from the container host. A missing device
or rejected mount returns a real error.

PlaySparse uses [`fuser` 0.18.0](https://github.com/cberner/fuser/tree/v0.18.0),
whose source supports a native Rust mount path on Linux with its `libfuse`
feature disabled. No libfuse development library is required by this Linux
configuration. The library's filesystem callbacks use 64-bit offsets and can
be served by multiple worker threads. PlaySparse configures 2–16 workers.

## macOS

Default macOS builds support packing and range reads without installing a
filesystem driver. They explicitly report mount support as unavailable. To
build the real macFUSE path:

```sh
brew install pkgconf
# Install the signed macFUSE 5.4 package from its official release page first.
cargo build --locked --release -p playsparse-cli --features macfuse
```

Follow the installed driver's kernel-backend setup instructions before mounting.
The supported range is macFUSE 5.3.3 or newer in the 5.x series; the SDK check
pins the signed 5.4.0 release. macFUSE 5.3 disabled the `fuse_mount_compat25`
entrypoint used by fuser 0.18. PlaySparse now mounts through public `fuse_mount`,
retains its channel and duplicates the borrowed `fuse_chan_fd` for
`Session::from_fd`. Ordinary OS unmount and channel destruction preserve single
ownership; neither fuser nor the application owns the original channel fd.

This transport uses the kernel backend. FSKit has no compatible device fd and
is explicitly unsupported; merely adding `backend=fskit` cannot supply the
missing channel transport. A physical macOS mount and executable test are
still required. See the
[macFUSE project](https://macfuse.github.io/) and
[fuser macOS dependency instructions](https://github.com/cberner/fuser/blob/v0.18.0/README.md#macos-untested),
[upstream compatibility issue](https://github.com/cberner/fuser/issues/752) and
[POSIX validation guide](posix-validation.md).

## Implemented behavior

- Stable inode metadata; lookup and directory enumeration, including empty directories.
- Regular-file open, arbitrary range reads, EOF, and concurrent requests.
- Release, flush, access checks, statfs and an empty extended-attribute namespace.
- Read-only mounts with execution enabled; write/create attempts are rejected by the kernel.
- Kernel page caching remains enabled so mmap and executable page faults use ordinary FUSE reads.
- An immutable decompressed-chunk cache shared by all workers, with range-layer single-flight loading.

File sizes and offsets remain 64-bit. File execute/read mode bits are preserved;
write and special privilege bits are removed. Directories use mode `0555`.
Ownership belongs to the mounting user, and fuser's default ACL limits access
to that user. The kernel performs permission checks. Timestamps are the Unix
epoch because storage format v1 does not preserve source timestamps. Symlinks,
alternate streams and source extended attributes are not in this format.
`statfs` and file block counts describe the logical view; use `pack`, `doctor`
and the store's disk usage to measure physical storage.

The configured cache bounds userspace decompressed resident chunk bytes. Active
read buffers and the OS page cache are additional memory. Kernel readahead may
request more than an application's individual read; it still requests bounded
ranges and does not extract the entire store.

On unmount, the process writes one JSON line to stderr with
`event: "fuse_unmounted"`, callback request/byte/error counts and chunk-cache
metrics. Kernel-cache hits can satisfy reads without a backend callback, so
these counters are distinct from application IO counts.

## Reproducible real mount test

Ordinary `cargo test -p playsparse-vfs-fuse` runs namespace validation, 64-bit
metadata, mountpoint validation, bounded chunk reads and corrupt-chunk-to-EIO
tests. The real mount test is explicitly ignored unless selected:

```sh
cargo test -p playsparse-vfs-fuse \
  real_mount_random_concurrent_mmap_and_native_execution \
  -- --ignored --nocapture
```

On macOS add `--features macfuse` before the test filter. The test fails if an
actual mount is unavailable. It generates a temporary application directory,
compiles a native C test program, packs both object layouts, mounts each store,
checks all bytes, tests ranges at chunk boundaries and EOF, runs concurrent
reads with 1/2/4/8/16 threads, reads mapped pages, verifies 1,200 directory
entries, launches the native program from the mounted directory, rejects
writes, unmounts and verifies that the source bytes and modes are unchanged.
Its first seven-byte mounted read also checks that fewer than the whole file's
bytes were decompressed. All generated files remain outside Git.

Sixteen simultaneous reads fault different pages of the same cold chunk before
the sequential read populates the kernel cache. The recorded run counted nine
single-flight waits in each layout, so this test also exercised shared
decompression from concurrent real FUSE callbacks. The
[captured test output](evidence/raw/fuse-backend-tests.log) includes the complete
unmount counters and cache metrics. Scheduling changes these counts across runs.

The test actually passed on Linux in the Docker development VM on 2026-10-02.
For each layout it printed `REAL_FUSE_TEST_OK` and the native program printed
`MOUNTED_EXECUTABLE_OK`. This is evidence for a Linux development mount, not a
Windows or macOS compatibility claim. The fixture had 2,168,088 logical bytes;
its packfile store had 174,244 physical bytes and its loose store had 174,236
physical bytes. These small-fixture sizes include metadata and depend on the
compiled executable. They are not game savings estimates or benchmark results.

For the development container used during this run:

```sh
docker exec -e CARGO_TARGET_DIR=/src/target-linux playsparse-runtime-dev \
  cargo test -p playsparse-vfs-fuse -- --include-ignored --nocapture
```

The container uses real Linux `/dev/fuse`; the repository is mounted at `/src`
and generated test data is under the container's `/tmp`.


## Writable and configured mounts

The default mount retains the existing read-only path. `--overlay` selects the
shared persistent Overlay engine through a separate callback adapter. The kernel
exercises create/write/truncate/chmod, directories, rename/replace, unlink,
flush/fsync and retained file handles. Mutable attributes have zero cache TTL.
Unlinked handles still support fstat/fchmod/read/write until their release;
`MAP_SHARED` writes and concurrent append were exercised by native FUSE tests.

Both adapters accept `--trace`, `--policy` and `--tiers`. Resolver prefetch stops
before final cache/tier metrics; the CLI drains and joins the trace writer after
mount teardown. Linux's kernel page cache may hide application reads from the
resolver and may generate larger or overlapping read-ahead requests.

Run the actual mounted scripts on a Linux machine with `/dev/fuse` permission:

```sh
python3 tools/mounted-update.py --work /tmp/playsparse-update-validation
python3 tools/adaptive-smoke.py --work /tmp/playsparse-adaptive-validation
python3 tools/tiered-smoke.py --work /tmp/playsparse-tiers-validation
```

Each requires an unused work directory, preserves raw logs and records actual
mount identity. The updater checks the complete remounted tree, immutable source
and base, immutable commit and reset. Tiers use verified local promotion and a
loopback HTTP range server; deliberately corrupt bytes produce EIO. Adaptive
capture/replay compares an identical trace and budget, including losing results.
See [sprint evidence](evidence/adaptive-writable-runtime.md).

macOS physical execution remains blocked without macFUSE. Portable host tests or
feature type checks do not establish physical mounting on macOS.
