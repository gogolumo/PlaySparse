# Windows filesystem backend

PlaySparse implements native read-only and experimental writable WinFsp paths in
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
driver; PlaySparse's mapped-read and executable-launch paths also passed the
native CI run recorded below. See [WinFsp compatibility](https://github.com/winfsp/winfsp/wiki/NTFS-Compatibility)
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
$installLog = Join-Path $env:TEMP 'winfsp-install.log'
$installer = Start-Process msiexec.exe -ArgumentList "/i `"$winfspMsi`" /qn ADDLOCAL=F.Main,F.User,F.Developer /norestart /l*v `"$installLog`"" -Wait -PassThru
if ($installer.ExitCode -notin @(0, 3010)) { throw "WinFsp installation failed: $($installer.ExitCode)" }
```

`ADDLOCAL` uses MSI feature IDs, not their display titles. The pinned installer
defines `F.Main`, `F.User` and `F.Developer` in its
[upstream WiX source](https://github.com/winfsp/winfsp/blob/v2.1/build/VStudio/installer/Product.wxs).
The CI workflow retains the verbose installer log even if a later step fails.

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

The default mount is read-only. It preserves file names and logical
sizes, while v1 manifests lack source timestamps and Windows ACLs; this backend
reports a fixed 2020-01-01 timestamp and a common read/execute descriptor.
Alternate streams, symlinks and per-file Windows permissions are unsupported.
Writable saves and synthetic updates use the separate overlay path described below. Names that collide under Windows ordinal case-insensitive
comparison or exceed the supported component/path lengths are rejected before
mount. Components are limited to 255 UTF-16 units; virtual paths, including the
leading slash and NUL, must fit WinFsp's 2,048-byte transaction limit. Windows
sizes above `i64::MAX` are rejected. Application writes require `--overlay` or a separate writable location. DRM and anti-cheat compatibility are not
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
C headers. On 2026-10-03, native MSVC workspace tests and release build passed
on hosted Windows Server 2025 with the pinned WinFsp 2.1 driver. The real directory
mount passed all 19 io-probe workloads, 177 executable read/mmap samples and
10 GiB offsets beyond 4/8 GiB. Source bytes stayed unchanged and the directory
mountpoint disappeared after unmount. See
[native raw evidence](evidence/raw/windows-native/result.json) and
[CI run](https://github.com/gogolumo/PlaySparse/actions/runs/37093219918).

The access descriptor uses concrete file read/execute rights (`FRFX`), rather
than generic ACE rights (`GRGX`), as required by the WinFsp user-mode access
check. [Microsoft's SDDL rights definitions](https://learn.microsoft.com/en-us/windows/win32/secauthz/ace-strings)
distinguish those masks. WOF comparisons and physical-Windows gaming tests remain
separate evidence gates. **WINDOWS HARDWARE TEST REQUIRED.**

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


## Writable overlay, tracing, policy and tiers

`mount BASE MOUNT --overlay OVERLAY` routes writes to the shared persistent engine;
base and source installations remain immutable. The writable path implements
Create/Open/Read/Write, overwrite, file-size changes, read-only attributes,
mkdir, rename/replace, delete-on-cleanup, directory enumeration and Flush.
One current-user security descriptor grants concrete file rights. Open handles
retain inode identity; cleanup cannot delete a replacement occupying an old name.
Unsupported explicit timestamps, arbitrary ACLs/reparse/EA and extra attributes
return explicit errors. Allocation requests are advisory, not an NTFS reservation.

The fixture performs ordinary application operations, unmounts/remounts and checks
all bytes. It then commits to a new verified immutable store and checks that view.
`--trace`, `--policy`, `--tiers` combine with this path; the local HTTP fixture needs
no Internet service. Run these scripts from the repository after a native build:

```powershell
python tools/mounted-update.py --work "$env:TEMP\playsparse-update-new" --playsparse target/release/playsparse.exe --io-probe target/release/io-probe.exe
python tools/adaptive-smoke.py --work "$env:TEMP\playsparse-adaptive-new" --playsparse target/release/playsparse.exe --io-probe target/release/io-probe.exe
python tools/tiered-smoke.py --work "$env:TEMP\playsparse-tiers-new" --playsparse target/release/playsparse.exe --io-probe target/release/io-probe.exe
```

A physical desktop validation harness records hardware, driver/tool versions,
Git SHA, source/base integrity and generated mounted workloads. It never installs
or approves drivers. From PowerShell 7 on an actual Windows client machine:

```powershell
.\tools\windows-hardware-validation.ps1 -EvidenceRoot "$env:TEMP\playsparse-hardware-new" -PhysicalMachine
```

Optional `-GamePath` plus a relative `-Executable` invokes a locally owned
application from a separate overlay with direct argument arrays. Assets remain
local and originals are fingerprinted. This is one filesystem compatibility run;
it does not establish Steam/Epic, DRM, anti-cheat or general game support.
Hosted Server CI remains distinct from physical Windows client validation.
See [sprint evidence](evidence/adaptive-writable-runtime.md),
[overlay](writable-overlay.md), [policy](adaptive-policy.md) and
[tiers](tiered-storage.md).

The combined overlay/policy/tier runtime subsequently passed hosted Server 2025
CI at `4e39b9c`: updater/remount, new-store commit and disposable-copy discard,
trace-derived adaptive replay, secondary promotion/offline remount, exact HTTP
ranges and corrupt-object rejection. See the
[sprint evidence](evidence/adaptive-writable-runtime.md#final-hosted-windows-execution).
These generated fixtures do not establish physical Windows desktop, real game
or launcher compatibility.
