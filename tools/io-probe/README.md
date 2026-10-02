# io-probe

A native ordinary-file-API reader independent of the PlaySparse resolver. Compare an original tree with a genuine mount:

```sh
cargo build --release -p io-probe
io-probe ORIGINAL MOUNTED --iterations 64 --output io-probe.json
```

It compares directory enumeration and logical file sizes, measures open/stat and sequential reads, checks random 4 KiB/64 KiB/1 MiB reads with 1/2/4/8/16 threads and maps the largest file to compare random pages. Requests include 256 KiB boundaries, offsets beyond 4 GiB and 8 GiB, truncated EOF and past-EOF reads when the file is large enough. Every returned byte must match. Successful results include ordered BLAKE3 digests, errors=0, throughput and p50/p95/p99. I/O errors or mismatches cause exit 1.

The sequential pass defaults to at most 64 MiB per file. Use `--sequential-limit 0` for full-file reads. `--baseline WOF_ROOT` adds a comparison against an already prepared NTFS/WOF tree; it does not configure Windows compression. OS caches are not cleared. Workloads may warm later requests, and summed concurrent operation throughput is distinct from wall throughput.

For a reproducible executable launch and colocated asset test:

```sh
python3 tools/generate-testgame.py /tmp/TestGame --io-probe target/release/io-probe
/tmp/TestGame/testgame --self-test
python3 tools/mounted-smoke.py --work /tmp/playsparse-mounted --iterations 64
```

The fixture defaults to a 10 GiB logical sparse world plus audio, config and assets. `testgame --self-test` resolves files next to its own executable and checks fixture bytes using both reads and memory maps. The mount runner launches the copy in the mount, requires an OS-confirmed filesystem mount, compares its checksums with the original, runs the probe, unmounts and fingerprints source extents before and after. It never extracts the store. Run directories and source bytes are preserved.

Mapped source files must remain unchanged while the probe runs; OS memory maps cannot safely tolerate concurrent truncation. The fixture runner enforces this condition for its generated source. On Windows pass `--playsparse target/release/playsparse.exe --io-probe target/release/io-probe.exe`; a directory mountpoint is created by WinFsp itself. Windows runtime acceptance requires the native WinFsp driver and a successful native run; a cross-compile alone is not evidence.

`memmap2 0.9.11` was selected against the [official crate documentation](https://docs.rs/memmap2/0.9.11/memmap2/) and [mapping safety requirements](https://docs.rs/memmap2/0.9.11/memmap2/struct.MmapOptions.html).
