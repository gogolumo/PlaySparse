# Original, WOF and PlaySparse on Windows

`tools/windows-comparison.py` compares verified reads through an original tree,
an independently copied WOF tree and a read-only PlaySparse WinFsp mount. It
retains the same deterministic `(path, offset, length)` requests and expected
SHA-256 values for every mode. These measurements cover file reads; application
startup, real games, launchers, DRM and anti-cheat remain `NOT RUN`.

## Run from the Windows harness

Use PowerShell 7 and the native prerequisites in [Windows backend setup](windows-backend.md).
The source and a new evidence root must be on the same local fixed NTFS volume,
outside Git and with neither tree containing the other. The combined harness
requires at least 32 GiB free; the comparison additionally checks for twice the
source's logical size plus 1 GiB before copying. Sparse originals can therefore
need considerably more free space than their current allocated size.

On a physical Windows client machine, after the driver has been installed:

```powershell
$run = 'C:\PlaySparseEvidence\comparison-' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss')
.\tools\windows-hardware-validation.ps1 -EvidenceRoot $run -PhysicalMachine `
    -WofComparison -WofSource 'C:\OwnedGames\Example' -WofAlgorithm XPRESS4K `
    -StageTimeoutSeconds 3600
Get-Content (Join-Path $run 'wof\result.json')
```

Omit `-PhysicalMachine` on hosted Windows or a VM. The requested software stages
can pass there, while `wof_physical_validation` and `physical_wof_validation`
remain `BLOCKED`. A physical result requires explicit user attestation, a
recorded Windows client OS, no detected VM hints and successful validation.
Absence of a VM hint alone is not hardware proof.

Without `-WofSource`, the comparison uses the harness's generated `TestGame`
source. That provides reproducible synthetic filesystem evidence and does not
turn into a real-game result. `-WofAlgorithm` accepts `XPRESS4K`, `XPRESS8K`,
`XPRESS16K` and `LZX`. Each invocation creates a separate copy; use a new evidence
root for every algorithm or repeated experiment.

## Repeat a comparison with a known binary

The harness writes `binary-build-manifest.json` after a successful workspace
build with an unchanged source digest. A standalone comparison requires that
receipt to match the current checkout and binary hash. Substitute paths from
your first harness run:

```powershell
python tools/windows-comparison.py `
    --work 'C:\PlaySparseEvidence\comparison-repeat-new' `
    --source 'C:\OwnedGames\Example' `
    --playsparse 'C:\src\PlaySparse\target\release\playsparse.exe' `
    --binary-manifest 'C:\PlaySparseEvidence\first-run\binary-build-manifest.json' `
    --environment 'C:\PlaySparseEvidence\first-run\environment.json' `
    --physical-machine --algorithm XPRESS4K --trials 3 --reads 300 --cache 64M
```

Use an environment report from the current machine and session for physical
evidence. A changed checkout or executable requires a new build receipt.
`--allow-dirty` and `--allow-unverified-binaries` are explicit local exploration
overrides recorded in the result; they cannot override a known receipt mismatch.
An unverified executable is not evidence that a particular revision was tested.

## What is measured

The default is three fresh-worker trials per mode, rotating the order
`original/WOF/PlaySparse`, `WOF/PlaySparse/original`, then
`PlaySparse/original/WOF`. Each worker performs one complete identical preload
before the timed replay. Windows/device caches are retained and uncontrolled;
the 64 MiB PlaySparse cache need not hold the complete request set. These are
repeated measurements after a preload, with no disk-cold claim.

| Field | Definition and limit |
| --- | --- |
| Logical bytes | Original file payload sizes; WOF must preserve them and all selected read bytes. |
| Allocated bytes | Sum of `FILE_STREAM_INFO.StreamAllocationSize` across every file data stream, including `WofCompressedData`; directory/MFT and other filesystem metadata excluded. |
| PlaySparse encoded bytes | Sum of sealed store file sizes; includes manifest/index metadata inside the store. |
| Read p50/p95/p99 | Per-request seek plus read; file opens and SHA-256 verification excluded from those latency samples. |
| Workload wall/CPU | Timed replay including file opens and byte verification; explicit preload excluded. |
| Client peak RSS | Fresh worker lifetime peak including its preload. |
| Provider CPU/RSS | WinFsp host CPU delta during timed replay and host lifetime peak working set. Windows kernel/driver CPU is not measured separately. |
| Read amplification | PlaySparse raw loaded bytes / driver returned bytes over preload plus timed replay. Native/WOF kernel read amplification is unmeasured and `null`. |

Each PlaySparse trial starts a new read-only host, verifies a directory volume
whose filesystem type is `PlaySparse`, unmounts it normally and checks teardown
and provider error metrics. Mounted contents must not appear as materialized
files after teardown. The source and sealed base fingerprints must match at the
end, including after failed commands. A failure or interrupt preserves logs and
disposable work rather than replacing the original or deleting failed evidence.

## WOF verification and retained evidence

The harness copies regular payloads with new file handles and no hardlinks.
`compact.exe /C /A /F /Q /EXE:<algorithm> /S:<disposable-copy> *` touches only
that copy. It does not run CompactOS or change global compression settings.
Logical contents are checked before and after compression.

Success from `compact.exe` alone does not establish WOF. Every externally backed
file must have `WOF_PROVIDER_FILE`, the requested algorithm and zero reserved
flags according to [`WofIsExternalFile`](https://learn.microsoft.com/en-us/windows/win32/api/wofapi/nf-wofapi-wofisexternalfile)
and [`WOF_FILE_COMPRESSION_INFO_V1`](https://learn.microsoft.com/en-us/windows/win32/api/wofapi/ns-wofapi-wof_file_compression_info_v1).
At least one verified WOF file is required. Empty or ineligible ordinary files
are counted separately. If the copy contains no actual WOF objects, the result
is `BLOCKED`, never a WOF success.

Sources with symlinks, junctions, unsupported reparse points or special files
are rejected. Named application streams are rejected because CAS cannot preserve
them; the known WOF backing stream is treated as storage representation. Windows
ACLs and source timestamps are outside the v1 mount metadata contract.

Retain `result.json`, `queries.json`, `expected.json`, source/base before-and-after
fingerprints, `wof-copy.json`, environment/build receipts, and command/provider
logs. Fingerprints and query names can themselves be private; review before
publishing. The original files, disposable copy and sealed stores stay local.
Report all trial results and cache definitions, including regressions, rather
than selecting a fastest sample.

Allocation comes from the public [`FILE_STREAM_INFO`](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_stream_info)
API; memory peaks use [`PROCESS_MEMORY_COUNTERS`](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters).
The native unit test compresses only a generated disposable temporary file and
checks those APIs on Windows. Portable test success, a cross-build or a hosted
Server run does not satisfy the physical Windows/WOF evidence gate.
