# Windows filesystem backend

PlaySparse implements a native, read-only WinFsp filesystem in
`crates/playsparse-vfs-win`. Each WinFsp `Read` callback calls the shared
`RangeResolver`, copies the returned bytes into the driver buffer, and retains
only decompressed chunks in the bounded cache. There is no unpack operation,
hydration directory, reconstructed file, or Python subprocess in this path.

## Why WinFsp

| Requirement | WinFsp native API | ProjFS |
| --- | --- | --- |
| Ordinary Windows file APIs | Filesystem driver and user-mode callbacks | NTFS placeholders and provider callbacks |
| Byte-range reads | `Read` receives a `u64` offset and a bounded output buffer | Provider receives requested offset/length |
| No materialized backing file | CAS bytes go directly into request buffers | `PrjWriteFileData` writes retrieved bytes into the local filesystem |
| Read-only namespace | Read-only volume plus shared read/execute descriptor | Requires policies around projected local files |
| Memory mapping | Supported by WinFsp's kernel filesystem | Hydration remains part of file-data delivery |
| Concurrent reads | Fine operation guards and dispatcher threads | Does not remove hydration requirement |

ProjFS can request ranges, but Microsoft's data-delivery contract populates
local files as their contents are accessed. That violates this project's
requirement to keep CAS as the only persistent representation. This is the
reason for choosing WinFsp, rather than a claim that ProjFS cannot handle random
reads. See [Microsoft's file-data contract](https://learn.microsoft.com/en-us/windows/win32/projfs/providing-file-data).

WinFsp documents memory-mapped I/O support. That establishes suitability of the
driver; PlaySparse's mapped-read and executable-launch compatibility still need
actual mount tests. See [WinFsp compatibility](https://github.com/winfsp/winfsp/wiki/NTFS-Compatibility)
and [native API reference](https://github.com/winfsp/winfsp/blob/master/doc/WinFsp-API-winfsp.h.md).

## Installation and native build

Use 64-bit Windows, Visual Studio Build Tools with the C++ toolchain and Windows
SDK, LLVM/libclang, and the repository's pinned Rust toolchain. Install WinFsp
including its **Developer** feature so `inc` and `lib` are available. The
WinFsp driver/DLL are required at runtime; a build alone does not provide them.
[The upstream tutorial explains the Developer installation](https://github.com/winfsp/winfsp/wiki/WinFsp-Tutorial).

The upstream stable release checked on 2026-10-02 was WinFsp 2.1.25156.
[Its release page](https://github.com/winfsp/winfsp/releases/tag/v2.1) publishes
the installer checksum used below. Run installation in an elevated terminal:

```powershell
$winfspMsi = Join-Path $env:TEMP 'winfsp-2.1.25156.msi'
Invoke-WebRequest 'https://github.com/winfsp/winfsp/releases/download/v2.1/winfsp-2.1.25156.msi' -OutFile $winfspMsi
if ((Get-FileHash $winfspMsi -Algorithm SHA256).Hash -ne '073A70E00F77423E34BED98B86E600DEF93393BA5822204FAC57A29324DB9F7A') { throw 'WinFsp installer checksum mismatch' }
$installer = Start-Process msiexec.exe -ArgumentList @('/i', $winfspMsi, '/qn', 'ADDLOCAL=Core,Developer') -Wait -PassThru
if ($installer.ExitCode -notin @(0, 3010)) { throw "WinFsp installation failed: $($installer.ExitCode)" }
```

From a Developer PowerShell terminal with LLVM installed:

```powershell
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
cargo build --locked --release --workspace
cargo test --locked --workspace
.\target\release\playsparse.exe doctor
```

`winfsp` 0.13.1+winfsp-2.1 and `winfsp-sys` 0.12.1+winfsp-2.1 are target-specific
dependencies. Their versions are fixed by Cargo.lock. `build.rs` in the CLI and
backend emits the architecture-specific delayed-DLL flags. Windows MSVC is the
supported native build target. [The Rust binding documentation](https://docs.rs/winfsp/0.13.1+winfsp-2.1/winfsp/)
describes the trait and delayed linking requirements.

## Mount and test

Create the generated corpus and keep its source directory:

```powershell
python tools/generate-testgame.py C:\PlaySparseTests\TestGame --io-probe target/release/io-probe.exe --world-bytes 10737418240
.\target\release\playsparse.exe pack C:\PlaySparseTests\TestGame C:\PlaySparseTests\TestGame.playsparse
.\target\release\playsparse.exe verify C:\PlaySparseTests\TestGame.playsparse
.\target\release\playsparse.exe mount C:\PlaySparseTests\TestGame.playsparse P: --cache 256M
```

The mount host stays in the foreground. In a second terminal:

```powershell
Get-Content P:\config.json
.\target\release\io-probe.exe C:\PlaySparseTests\TestGame P:\ --iterations 64 --output C:\PlaySparseTests\io-probe.json
P:\testgame.exe --self-test
.\target\release\playsparse.exe unmount P:
```

Check the generated fixture's executable name if it differs. `io-probe` compares
directory listings, sizes, ordinary reads, offsets beyond 4 and 8 GiB, EOF,
1/2/4/8/16 reader threads, and random mapped pages. A failed byte comparison or
mapped access is a compatibility failure. A successful run must preserve the
source and provide its JSON evidence before claiming a Windows milestone.

A directory such as `C:\PlaySparseTests\Mounted` can replace `P:`. Use a path
that does **not exist**, with an existing writable parent. WinFsp creates the
directory mountpoint and removes it during teardown. See
[the WinFsp mountpoint FAQ](https://github.com/winfsp/winfsp/wiki/Frequently-Asked-Questions).

Ctrl+C and `unmount` both trigger host teardown; `unmount` waits for its
completion, with a 60-second timeout. Named events are scoped to the current
Windows session. The host's exit prints chunk-cache metrics. Windows page cache
and read-ahead may cause additional driver requests; the resolver decompresses
only chunks intersecting each received request.

## Current limits and validation

The initial implementation is read-only. It preserves file names and logical
sizes, while v1 manifests lack source timestamps and Windows ACLs; this backend
reports a fixed 2020-01-01 timestamp and a common read/execute descriptor.
Alternate streams, symlinks, writable saves, and per-file Windows permissions
are unsupported. Names that collide under Windows ordinal case-insensitive
comparison or exceed the supported component/path lengths are rejected before
mount. Components are limited to 255 UTF-16 units; virtual paths, including the
leading slash and NUL, must fit WinFsp's 2,048-byte transaction limit. Windows
sizes above `i64::MAX` are rejected. Application writes must go
to a separate writable location. DRM and anti-cheat compatibility are not
inferred from WinFsp's generic file API support.

Security descriptor size queries return the required size. An undersized
provided buffer fails with `STATUS_BUFFER_OVERFLOW` and is left untouched. The
current Rust binding cannot update the required-size output on an error; callers
that need a larger buffer can query size with a null descriptor first. This
binding limitation needs native Windows coverage for uncommon security callers.

Source checks completed on the development Mac: the ordinary non-Windows crate
build passed; Windows backend and test source passed a Windows-target Rust type
check against the real bindings' pregenerated documentation API. That limited
check used an external temporary Cargo manifest, disabled `system` registry
discovery, and did not link or execute Windows code. The normal cross-build
failed before the backend because the Mac lacks MSVC assembly tools and Windows
C headers. Native Windows build, mounted I/O, mapped executable launch, WOF
comparisons, and physical-Windows gaming tests remain separate evidence gates.

## Dependency licenses

PlaySparse's own source retains its MIT license. The Rust `winfsp` and
`winfsp-sys` package metadata declares `GPL-3.0`, so a Windows executable that
includes them must account for those dependency terms; the workspace's MIT
field is not a license inventory for the combined executable.

WinFsp itself has GPLv3 terms and a specific FLOSS linking/distribution
exception with conditions. That DLL exception does not establish an exception
for separately authored Rust bindings. Consult [WinFsp's actual license](https://github.com/winfsp/winfsp/blob/master/License.txt)
and the binding package licenses when distributing binaries.
The [Rust binding's own license](https://github.com/SnowflakePowered/winfsp-rs/blob/master/LICENSE.md)
is separately provided by its upstream repository.

WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos.
Upstream: [winfsp/winfsp](https://github.com/winfsp/winfsp).
