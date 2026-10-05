# PlaySparse — Windows Physical Hardware Validation & Storage Analysis Report

## Purpose

This document summarizes physical Windows testing of PlaySparse, including:

- Windows build/test validation;
- WinFsp validation on real hardware;
- writable/read-only/adaptive/tiered testing;
- real-game testing with Elden Ring;
- comparison against Windows WOF XPRESS4K;
- real storage measurements;
- full `playsparse analyze` results;
- Windows TEMP/TMP issue discovered during analysis;
- recommended next development priorities.

The major conclusion is:

> **PlaySparse's Windows filesystem implementation is functioning successfully on physical hardware. The main unresolved problem is achieving substantially better storage reduction on real modern games.**

---

# 1. Test Environment

Physical Windows machine:

```text
OS: Windows 11 Pro
Architecture: x64
CPU: Intel Core i7-12700H
CPU topology: 14 cores / 20 logical processors
RAM: ~16 GB
Storage: Samsung NVMe SSD
Filesystem: NTFS

PowerShell: 7.6.6
Python: 3.13.15
Rust: 1.99.0
LLVM/Clang: 23.1.2
Visual Studio Build Tools: 2022 / MSVC
WinFsp: 2.1.25156
Git: 2.55.0.windows.5
```

Repository revision tested:

```text
7783946b1bde21f78a40ac6711bab3e47caedf65
```

Repository was clean during validation.

PlaySparse SHA-256:

```text
d66682947e92f496054cda6f59031be20414c1150c130a04f033a0d9cd8a6b02
```

`io-probe.exe` SHA-256:

```text
e918c6da9b337bfd0949ccfa60e68018165054628d454d790d4a5862e3cf9b4a
```

Development/test data was intentionally kept primarily under:

```text
D:\PlaySparse
```

---

# 2. Build and Workspace Tests

The complete workspace test suite was run:

```powershell
cargo test --locked --workspace
```

All observed tests passed.

This included tests for:

- cache;
- CLI;
- core;
- overlay;
- policy;
- ranges;
- store;
- trace;
- FUSE-related code;
- Windows VFS;
- offsets greater than 8 GiB;
- Windows security descriptors;
- read callbacks;
- case-insensitive lookup;
- writable behavior;
- remount behavior.

Release build:

```powershell
cargo build --locked --release --workspace
```

Result:

```text
PASS
```

Produced:

```text
target\release\playsparse.exe
target\release\io-probe.exe
```

---

# 3. WinFsp

WinFsp version tested:

```text
2.1.25156
```

WinFsp launcher service:

```text
Running
```

WinFsp DLL loading succeeded.

The PlaySparse Windows backend successfully created real WinFsp mounts during physical validation.

This means the Windows backend has now been exercised on actual Windows hardware rather than only through unit tests/CI.

---

# 4. `doctor --mount-test` on Windows

Running:

```powershell
.\target\release\playsparse.exe doctor --mount-test
```

returned:

```text
exit code: 2
mount_test.status: BLOCKED
```

This was not a WinFsp failure.

The disposable doctor mount probe currently supports POSIX systems and instructs Windows users to use:

```text
tools\windows-hardware-validation.ps1
```

instead.

## Recommended improvement

On Windows, `doctor` should make this distinction extremely obvious.

For example:

```text
Windows detected.

WinFsp backend ........ AVAILABLE
WinFsp DLL ............ LOADED
Elevation ............. YES

The generic disposable doctor mount probe is POSIX-only.

For native Windows mount validation run:

pwsh -File tools/windows-hardware-validation.ps1 ...
```

Longer-term, `doctor --mount-test` should ideally perform a minimal WinFsp mount test itself.

---

# 5. Physical Windows Hardware Validation

The physical validation was run with:

```powershell
.\tools\windows-hardware-validation.ps1 `
    -EvidenceRoot <evidence-directory> `
    -PhysicalMachine
```

Result:

```text
status: PASS
exit code: 0
```

Stages:

```text
build       PASS
doctor      PASS
readonly    PASS
writable    PASS
adaptive    PASS
tiers       PASS
```

Physical validation reported:

```text
user_attested_physical_machine: true
virtual_machine_detected: false
```

The current evidence wording correctly avoids claiming that lack of detected VM indicators constitutes automatic proof of physical hardware.

That behavior should remain.

---

# 6. Real Game Validation — Elden Ring

Real game installation:

```text
D:\Elden Ring\Elden Ring
```

Executable:

```text
eldenring.exe
```

Validation:

```powershell
.\tools\windows-hardware-validation.ps1 `
    -EvidenceRoot <evidence-directory> `
    -PhysicalMachine `
    -GamePath "D:\Elden Ring\Elden Ring" `
    -Executable "eldenring.exe" `
    -StageTimeoutSeconds 3600
```

The game:

- launched successfully;
- reached its main menu;
- remained running normally;
- exited normally.

Result:

```text
status: PASS
exit code: 0
```

Application stage:

```text
PASS
```

The validation scope was correctly recorded as:

```text
filesystem compatibility for this executable and arguments only;
no launcher/DRM/anti-cheat claim
```

This distinction is important.

This test does NOT establish compatibility with:

- Steam launch paths;
- launcher integration;
- DRM;
- Easy Anti-Cheat;
- protected multiplayer.

Those were not tested.

---

# 7. WOF Comparison

A physical Windows WOF comparison was performed with:

```text
WOF algorithm: XPRESS4K
```

Source:

```text
D:\Elden Ring\Elden Ring
```

Result:

```text
status: PASS
```

WOF external backing was verified using:

```text
WofIsExternalFile
```

Result:

```text
status: VERIFIED
algorithm: XPRESS4K
```

The comparison also verified real PlaySparse WinFsp mounts:

```text
filesystem_type: PlaySparse
volume_label: PlaySparse
real_mount: true
```

---

# 8. WOF / PlaySparse Storage Results

Game logical size:

```text
71,251,803,124 bytes
```

Approximately:

```text
66.36 GiB
```

Original allocated storage:

```text
71,251,944,504 bytes
```

WOF XPRESS4K allocated storage:

```text
71,186,887,736 bytes
```

PlaySparse allocated storage:

```text
68,947,853,496 bytes
```

Approximate comparison:

| Representation | Allocated | Saving vs original |
|---|---:|---:|
| Original | ~66.36 GiB | — |
| WOF XPRESS4K | ~66.29 GiB | ~0.06 GiB |
| PlaySparse CDC | ~64.21 GiB | ~2.15 GiB |

PlaySparse saves approximately:

```text
~2.15 GiB
~3.2%
```

relative to the original installation.

PlaySparse also saves approximately:

```text
~2.09 GiB
```

more than WOF XPRESS4K.

---

# 9. Full `playsparse analyze` Test

The following command was run:

```powershell
.\target\release\playsparse.exe analyze "D:\Elden Ring\Elden Ring"
```

After fixing the Windows temporary-directory issue described later in this report, analysis completed successfully.

The analysis was a:

```text
measured full scan, two temporary verified stores; no extrapolation
```

The source was not modified:

```text
source_modified: false
```

Temporary stores were removed:

```text
temporary_stores_removed_on_exit: true
```

---

# 10. CDC Analysis Results

CDC result:

```text
logical_bytes:
71,251,803,124

physical_bytes:
68,947,338,459

object_bytes:
68,909,781,984

metadata_bytes:
37,556,475

files:
70

filesystem_entries:
264

unique_objects:
216,894

reused_chunks:
5,737

pack_seconds:
986.786

verified_before_publish:
true
```

Approximate physical size:

```text
64.21 GiB
```

---

# 11. Fixed-Chunk Analysis Results

The same source was also measured using fixed chunks.

Result:

```text
logical_bytes:
71,251,803,124

physical_bytes:
70,746,318,189

object_bytes:
70,700,052,424

metadata_bytes:
46,265,765

files:
70

filesystem_entries:
271

unique_objects:
271,853

reused_chunks:
0

pack_seconds:
1068.663

verified_before_publish:
true
```

Approximate physical size:

```text
~65.89 GiB
```

---

# 12. CDC Is Clearly Better Than Fixed Chunking

This real-world test demonstrates that content-defined chunking is providing meaningful benefit.

Fixed chunking:

```text
physical:
70,746,318,189 bytes

reused chunks:
0
```

CDC:

```text
physical:
68,947,338,459 bytes

reused chunks:
5,737
```

Difference:

```text
~1.68 GiB
```

in physical representation.

Therefore CDC should remain the primary strategy for this workload.

Going back to conventional fixed-size chunking would significantly reduce storage savings.

---

# 13. Duplicate Chunk Reuse

The analysis directly measured:

```text
cdc_duplicate_reuse_bytes:
1,693,751,846
```

Approximately:

```text
1.58 GiB
```

of raw data can therefore be reused through CDC duplicate detection.

This is one of the most important findings from the analysis.

The game contains meaningful repeated byte sequences even though there are no exact duplicate files.

Exact duplicate file result:

```text
exact_duplicate_file_bytes:
0
```

Therefore:

> Whole-file duplicate detection would provide essentially no benefit, but chunk-level deduplication provides substantial benefit.

---

# 14. Compressible vs Incompressible Raw Data

The analyzer measured:

```text
unique_compressible_raw_bytes:
8,042,562,746
```

Approximately:

```text
7.49 GiB
```

It measured:

```text
unique_incompressible_raw_bytes:
61,515,488,532
```

Approximately:

```text
57.29 GiB
```

This is the most important result for future storage work.

The majority of the Elden Ring installation is currently classified by the codec measurement as unique data that the current compression path does not beneficially compress.

Approximate breakdown:

```text
Unique compressible raw data:   ~7.49 GiB
Unique incompressible raw data: ~57.29 GiB
CDC duplicate reuse:            ~1.58 GiB
```

---

# 15. Important Classification Caveat

The analyzer explicitly reports:

```text
Codec choice is measured. Already-compressed and high-entropy
attribution is not inferred from filename or compression ratio.
```

Additionally:

```text
already_compressed_bytes: null
high_entropy_bytes: null
```

Therefore we should NOT conclude yet that the ~57 GiB is mathematically random or fundamentally impossible to optimize.

What has been demonstrated is narrower:

> The current generic codec does not find beneficial compression for approximately 57 GiB of unique raw input.

That data could still contain higher-level redundancy that ordinary block compression cannot exploit.

---

# 16. The 12 GiB Target

The desired storage reduction is approximately:

```text
10–12+ GiB
```

12 GiB on a ~66.36 GiB installation corresponds to approximately:

```text
18%
```

reduction.

Current measured PlaySparse saving:

```text
~2.15 GiB
~3.2%
```

Therefore another approximately:

```text
~9.8 GiB
```

would need to be eliminated to reach the 12 GiB target.

The current analysis strongly suggests that simply changing cache size or slightly increasing generic compression level will not produce that improvement.

The main opportunity must come from understanding the ~57 GiB currently treated as incompressible unique data.

---

# 17. Large Game Archives Are Now the Primary Research Target

Elden Ring contains large archive/container files including:

```text
Data0.bdt
Data1.bdt
Data2.bdt
Data3.bdt
DLC.bdt
```

WOF was unable to substantially reduce the installation.

PlaySparse's generic CDC/compression performs much better than WOF, but still only saves ~3.2%.

The next question is:

> How much of the ~57 GiB of currently incompressible unique data resides inside large BDT archives?

And then:

> Is that data genuinely already compressed/high entropy, or is useful semantic redundancy hidden inside the archive format?

This should be the next major investigation.

---

# 18. Recommended Per-File Analysis

`playsparse analyze` should gain an optional detailed per-file mode.

For example:

```powershell
playsparse analyze GAME --per-file
```

Output should include:

```text
path
logical bytes
physical encoded bytes
compressible raw bytes
incompressible raw bytes
duplicate/reused bytes
chunk count
unique chunks
reused chunks
compression ratio
dedupe ratio
metadata overhead
```

Example:

```text
File                  Logical   Encoded   Reused   Compressible
----------------------------------------------------------------
Data0.bdt             18.2 GiB   17.9 GiB  200 MiB   400 MiB
Data1.bdt             12.4 GiB   11.8 GiB  450 MiB   600 MiB
...
```

This would immediately identify where further engineering effort is worthwhile.

---

# 19. Add File-Type / Extension Aggregation

Analysis should also optionally aggregate by extension:

```text
Extension    Logical    Encoded    Saved    Reused
---------------------------------------------------
.bdt         ...
.bhd         ...
.dll         ...
.exe         ...
.bik         ...
.bank        ...
```

This would make it much easier to identify which classes of game data dominate storage.

---

# 20. Consider Archive-Aware Storage

If the large `.bdt` files are already compressed internally, generic filesystem compression may never reach ~18%.

However, that does not necessarily mean the target is impossible.

A more advanced PlaySparse representation could potentially understand container boundaries and store internal resources separately while reconstructing the exact original byte stream through the virtual filesystem.

Conceptually:

```text
Game asks for:

Data0.bdt offset X length Y

        ↓

PlaySparse virtual representation

        ↓

reconstruct required archive bytes

        ↓

application receives exactly the bytes expected
```

Potential benefits:

- deduplicate assets inside containers;
- deduplicate related archive resources;
- eliminate redundant internal representations;
- avoid storing repeated resources multiple times;
- potentially exploit archive structure that generic CDC cannot see.

Any such implementation must preserve exact application-visible bytes.

---

# 21. Do Not Optimize Around DRM / Anti-Cheat

Archive-aware storage should remain a filesystem/storage optimization.

It should not depend on:

- modifying the game executable;
- disabling DRM;
- bypassing anti-cheat;
- patching protected launchers.

The existing filesystem-compatibility scope should remain clearly separated from launcher/DRM/anti-cheat compatibility.

---

# 22. Compression Experiments Still Worth Running

Generic compression experiments are still useful, but expectations should be realistic.

Possible profiles:

```text
fast
balanced
maximum
```

Potential experiments:

```text
zstd level 3
zstd level 6
zstd level 9
zstd level 15+
```

Also investigate:

- dictionaries;
- dictionary training;
- grouping similar chunks;
- larger compression windows;
- adaptive compression levels;
- skipping obviously non-beneficial compression attempts.

But these should be benchmarked against the measured baseline.

The important metric is not merely compression ratio.

Measure:

```text
pack time
encoded bytes
allocated bytes
decompression latency
CPU usage
provider RSS
read amplification
mount performance
```

---

# 23. CDC Parameter Experiments

CDC is clearly useful on this game.

It is worth testing whether its current boundaries are optimal.

Potential average chunk sizes:

```text
64 KiB
128 KiB
256 KiB
512 KiB
1 MiB
```

For every configuration measure:

```text
physical bytes
duplicate reuse bytes
unique object count
metadata bytes
pack time
provider memory
random-read latency
sequential-read throughput
```

The goal is to find the best total tradeoff rather than maximizing deduplication alone.

---

# 24. Potential Similarity Deduplication

Current CDC detects exact duplicate chunks.

Another research direction is similarity-aware storage.

For example, two chunks may be:

```text
95% identical
```

but still require two complete compressed objects because their hashes differ.

Potential future techniques:

- delta encoding;
- similarity hashing;
- base-object + patch storage;
- rolling-hash similarity detection;
- archive-resource-aware deltas.

This is significantly more complex than ordinary deduplication and should only be pursued if analysis demonstrates enough near-duplicate data to justify it.

---

# 25. Windows TEMP/TMP Failure Discovered

The first attempt to run:

```powershell
.\target\release\playsparse.exe analyze "D:\Elden Ring\Elden Ring"
```

failed with:

```text
Error: I/O: There is not enough space on the disk. (os error 112)
```

At the time:

```text
D: free:
259.62 GiB
```

Therefore the obvious interpretation that the game/work drive was full was incorrect.

Environment:

```text
C: free:
14.24 GiB

D: free:
259.62 GiB
```

TEMP and TMP were:

```text
C:\Users\<user>\AppData\Local\Temp
```

---

# 26. TEMP/TMP Workaround Confirmed

A temporary directory was created on D:

```powershell
New-Item -ItemType Directory -Force "D:\PlaySparse\Temp" | Out-Null

$env:TEMP = "D:\PlaySparse\Temp"
$env:TMP  = "D:\PlaySparse\Temp"
```

Environment then became:

```text
TEMP = D:\PlaySparse\Temp
TMP  = D:\PlaySparse\Temp
```

with:

```text
D free = 259.62 GiB
```

The exact same analyze command was then rerun:

```powershell
.\target\release\playsparse.exe analyze "D:\Elden Ring\Elden Ring"
```

Result:

```text
SUCCESS
```

The full two-store measured analysis completed.

This is strong evidence that the earlier Windows `os error 112` was associated with the temporary storage location rather than insufficient free space on the source/work drive.

---

# 27. This Should Be Fixed in PlaySparse

This is a significant Windows usability issue.

A user can have hundreds of GiB free on the game drive but receive:

```text
There is not enough space on the disk.
```

because `%TEMP%` points to a constrained system drive.

PlaySparse should support:

```text
--temp-dir <path>
```

For example:

```powershell
playsparse analyze `
    "D:\Elden Ring\Elden Ring" `
    --temp-dir "D:\PlaySparse\Temp"
```

Ideally this should apply consistently to all large operations.

---

# 28. Better Disk-Space Error Messages

Instead of:

```text
There is not enough space on the disk. (os error 112)
```

report:

```text
Insufficient temporary storage.

Temporary directory:
C:\Users\user\AppData\Local\Temp

Temporary volume:
C:\

Available:
14.24 GiB

Source:
D:\Elden Ring\Elden Ring

Source volume available:
259.62 GiB

This operation requires temporary stores that may approach
the size of the source data.

Use:

--temp-dir D:\PlaySparse\Temp
```

This would have diagnosed the problem immediately.

---

# 29. Analyze Should Perform Storage Preflight

Before building two temporary verified stores, `analyze` already knows that the operation may require significant temporary storage.

It should perform a preflight.

Example:

```text
PlaySparse Analysis Preflight

Source logical size:
66.36 GiB

TEMP:
C:\Users\user\AppData\Local\Temp

TEMP volume:
C:\

Available:
14.24 GiB

Estimated temporary requirement:
> 130 GiB

ERROR:
Insufficient temporary storage.

Suggested volume:

D:\
available 259.62 GiB
```

This is preferable to spending time processing data and failing later.

---

# 30. Analyze Uses Significant Temporary Storage

The current analysis description says:

```text
measured full scan, two temporary verified stores; no extrapolation
```

This is good methodology, but it means storage requirements can be substantial.

For a ~66 GiB game, two temporary stores can require well over 100 GiB depending on representation and filesystem allocation.

This should be documented in:

```text
playsparse analyze --help
```

For example:

```text
WARNING:
Full analysis creates two temporary verified stores.

Temporary disk usage can approach approximately 2x the
logical source size.

Use --temp-dir to select a volume with sufficient space.
```

---

# 31. Analyze Performance

Measured pack times:

CDC:

```text
986.786 seconds
~16.4 minutes
```

Fixed:

```text
1068.663 seconds
~17.8 minutes
```

Total analysis therefore performs substantial work.

That is acceptable for a full measurement mode, but the CLI should make the distinction between:

```text
quick estimate
```

and:

```text
full measured analysis
```

possible in the future.

For example:

```powershell
playsparse analyze GAME --quick
```

versus:

```powershell
playsparse analyze GAME --full
```

`--full` should remain the authoritative mode.

---

# 32. Performance Comparison

The earlier WOF comparison performed 300 verified reads requesting:

```text
54,987,548 bytes
```

per trial.

Trial order was randomized:

```text
Trial 0:
Original -> WOF -> PlaySparse

Trial 1:
WOF -> PlaySparse -> Original

Trial 2:
PlaySparse -> Original -> WOF
```

Approximate mean wall times:

```text
Original:   ~49.6 ms
WOF:        ~52.3 ms
PlaySparse: ~57.2 ms
```

For this specific warm workload:

```text
PlaySparse vs WOF:
~9% slower

PlaySparse vs original:
~15% slower
```

Absolute differences remain small.

This benchmark should NOT be described as disk-cold.

---

# 33. Representative Read Latency

Representative trial:

```text
Original

p50:   9.1 us
p95: 318.8 us
p99: 360.7 us
```

WOF:

```text
p50:   9.5 us
p95: 316.2 us
p99: 371.5 us
```

PlaySparse:

```text
p50:   9.9 us
p95: 315.1 us
p99: 357.8 us
```

The latency distribution therefore remained reasonably competitive.

There was no catastrophic WinFsp random-read latency penalty in this workload.

---

# 34. Cache Measurements

Representative PlaySparse metrics:

```text
decompressions:
267

decompressions avoided:
48

cache hits:
48

cache misses:
267

cache hit ratio:
~15.24%

evictions:
29
```

Peak cache residency:

```text
~67 MB
```

Provider peak RSS:

```text
~147–149 MB
```

Configured cache:

```text
64M
```

---

# 35. Prefetch Opportunity

Measured prefetch activity:

```text
prefetch_requests:       0
prefetch_chunks_loaded:  0
prefetch_bytes_loaded:   0
prefetch_hits:           0
```

Prefetch therefore remains a potential runtime optimization.

Future tests:

```text
none
next chunk
next 2 chunks
next 4 chunks
adaptive sequential detection
trace-driven prefetch
```

Measure:

```text
latency
wall time
provider CPU
RSS
cache hit ratio
useful prefetch bytes
wasted prefetch bytes
read amplification
```

---

# 36. WOF Result Is Also Informative

WOF XPRESS4K saved only approximately:

```text
~0.06 GiB
```

on the installation.

This strongly suggests that conventional transparent Windows compression has very little opportunity on the dominant game data.

PlaySparse's ~2.15 GiB saving is substantially better because CDC can exploit repeated byte sequences that WOF cannot.

Therefore:

> Deduplication appears substantially more important than ordinary filesystem compression for this particular game.

---

# 37. Recommended Windows Preflight

Add:

```powershell
playsparse preflight
```

or:

```powershell
playsparse doctor --windows
```

Check:

```text
Windows version
architecture
WinFsp version
WinFsp DLL
WinFsp Launcher
elevation
PowerShell version
TEMP
TMP
TEMP volume free space
source volume
destination volume
work/evidence volume
filesystem type
NTFS
available storage
repository/build provenance
```

Example:

```text
PlaySparse Windows Preflight

Windows 11 x64 ................. PASS
NTFS ........................... PASS
WinFsp 2.1.25156 ............... PASS
WinFsp DLL ..................... PASS
WinFsp Launcher ................ PASS
Administrator .................. PASS

Source volume D: ............... PASS
259.62 GiB free

TEMP volume C: ................. WARN
14.24 GiB free

WARNING:
Large PlaySparse operations may fail because TEMP is located
on a low-space system volume.

Recommended:
--temp-dir D:\PlaySparse\Temp
```

---

# 38. Validation Script Should Control TEMP

`windows-hardware-validation.ps1` should optionally create:

```text
<EvidenceRoot>\temp
```

and set:

```powershell
$env:TEMP
$env:TMP
```

for child processes.

Alternatively expose:

```text
-TempRoot
```

Example:

```powershell
.\tools\windows-hardware-validation.ps1 `
    -EvidenceRoot D:\PlaySparse\Evidence\run `
    -TempRoot D:\PlaySparse\Temp `
    -PhysicalMachine
```

---

# 39. Disk Budget Reporting

Before WOF or analysis operations, report:

```text
Source logical size
Expected WOF-copy size
Expected PlaySparse-store size
Expected analysis-store size
Evidence reserve
TEMP reserve
Required free space
Available free space
```

Do this before performing expensive work.

---

# 40. Large Artifact Cleanup

WOF comparison creates very large temporary/generated data.

Provide:

```text
-CleanupLargeArtifacts
```

or:

```text
-KeepArtifacts
```

Small evidence should remain:

```text
result.json
environment.json
logs
hashes
manifests
performance summaries
storage summaries
```

while huge reproducible working copies can optionally be deleted.

---

# 41. Evidence JSON Should Include Derived Storage Results

Instead of requiring users to calculate differences manually:

```json
{
  "storage_comparison": {
    "original_allocated_bytes": 71251944504,
    "wof_allocated_bytes": 71186887736,
    "playsparse_allocated_bytes": 68947853496,

    "wof_saved_bytes": 65056768,
    "playsparse_saved_bytes": 2304091008,

    "wof_saved_percent": 0.09,
    "playsparse_saved_percent": 3.23,

    "playsparse_vs_wof_saved_bytes": 2239034240
  }
}
```

---

# 42. Analysis JSON Should Include Derived Values

The new `analyze` output is already very useful.

It could become even more useful by adding:

```json
{
  "summary": {
    "logical_gib": 66.36,
    "cdc_physical_gib": 64.21,
    "fixed_physical_gib": 65.89,

    "cdc_saved_gib": 2.15,
    "cdc_saved_percent": 3.2,

    "duplicate_reuse_gib": 1.58,

    "unique_compressible_gib": 7.49,
    "unique_incompressible_gib": 57.29
  }
}
```

The raw byte counts should remain authoritative.

---

# 43. Highest-Priority Storage Investigation

The next development sequence should be:

1. Determine which files account for the ~57.29 GiB of unique incompressible raw data.

2. Measure how much of that is `.bdt`.

3. Determine whether BDT payloads are internally compressed/encrypted/high-entropy or simply resistant to the current codec.

4. Measure duplication inside archive structures.

5. Measure near-duplication/similarity, not only exact CDC duplicates.

6. Test alternate CDC parameters.

7. Test stronger compression/dictionaries.

8. Determine the maximum realistic generic storage saving.

9. Only then decide whether archive-aware virtualization is justified.

This avoids spending engineering time chasing an arbitrary 12 GiB target if the underlying information content makes that impossible.

---

# 44. Suggested Next Analysis Command

A future CLI should support something like:

```powershell
playsparse analyze `
    "D:\Elden Ring\Elden Ring" `
    --temp-dir D:\PlaySparse\Temp `
    --per-file `
    --extensions `
    --entropy `
    --similarity `
    --output analysis.json
```

This could answer the complete storage question in one run.

---

# 45. What Is Already Proven

Current physical Windows results:

```text
Rust workspace tests .............. PASS
Release build ..................... PASS

WinFsp detection .................. PASS
WinFsp DLL loading ................ PASS
Real WinFsp mounts ................ PASS

Read-only validation .............. PASS
Writable validation ............... PASS
Adaptive validation ............... PASS
Tiered validation ................. PASS

Physical Windows validation ....... PASS

Real Elden Ring executable ........ PASS

WOF XPRESS4K comparison ........... PASS
WOF external backing verification . PASS

PlaySparse source integrity ....... PASS
Cleanup ........................... PASS

Full measured analyze ............. PASS
CDC verified store ................ PASS
Fixed verified store .............. PASS
Source unchanged .................. PASS
```

This is strong evidence that the Windows implementation itself is functioning.

---

# 46. What Is NOT Proven

The testing does NOT currently establish:

```text
Steam launcher compatibility
Easy Anti-Cheat compatibility
DRM compatibility
protected multiplayer compatibility
true disk-cold performance
universal compatibility with Windows games
12 GiB savings on Elden Ring
```

Those should not be claimed.

---

# 47. Key New Finding From `analyze`

The most important new result is:

```text
Logical source:
71,251,803,124 bytes

CDC physical:
68,947,338,459 bytes

Fixed physical:
70,746,318,189 bytes

CDC duplicate reuse:
1,693,751,846 bytes

Unique compressible raw:
8,042,562,746 bytes

Unique incompressible raw:
61,515,488,532 bytes
```

This substantially changes the storage investigation.

The problem is no longer simply:

> "Can we increase the compression level?"

Instead it is:

> "What exactly are the ~61.5 billion bytes that the current codec rejects as non-beneficial to compress, and can PlaySparse exploit their structure at a higher semantic level?"

That should be the primary storage research question.

---

# 48. Key New Windows Finding

The successful analysis also demonstrates a reproducible Windows usability issue:

```text
TEMP on C:
14.24 GiB free

analyze:
FAILED with os error 112
```

After:

```text
TEMP on D:
259.62 GiB free
```

the same command:

```text
playsparse analyze "D:\Elden Ring\Elden Ring"
```

completed successfully.

Therefore Windows temporary-storage handling should be treated as a real product issue, not merely a setup/documentation detail.

---

# 49. Recommended Priority Order

## P0 — Windows robustness

1. Add `--temp-dir`.
2. Preflight TEMP/TMP free space.
3. Report the actual failing volume/path for error 112.
4. Document full-analysis temporary storage requirements.

## P1 — Storage observability

5. Add per-file analysis.
6. Add extension/type aggregation.
7. Report derived GiB/percentage values.
8. Identify where incompressible bytes reside.
9. Identify where duplicate reuse comes from.

## P2 — Storage efficiency

10. Tune CDC parameters.
11. Benchmark stronger compression modes.
12. Investigate dictionaries.
13. Investigate similarity/delta storage.
14. Analyze BDT internal structure.
15. Evaluate archive-aware virtualization.

## P3 — Runtime

16. Investigate prefetch.
17. Improve cache hit ratio.
18. Benchmark multiple cache sizes.
19. Add application startup measurements.
20. Improve cold/warm benchmark methodology.

## P4 — Windows UX

21. Improve `doctor --mount-test`.
22. Add Windows preflight.
23. Improve WinFsp diagnostics.
24. Improve disk-budget reporting.
25. Add optional large-artifact cleanup.

---

# 50. Overall Assessment

PlaySparse has now passed meaningful physical Windows testing.

The Windows/WinFsp implementation appears functional enough to:

- mount real stores;
- service real reads;
- support writable behavior;
- survive adaptive/tiered validation;
- run a real Elden Ring executable;
- complete WOF comparison workloads;
- maintain source integrity;
- perform full measured CDC/fixed analysis.

The largest remaining question is no longer whether PlaySparse works on Windows.

The largest question is:

> **Can PlaySparse exploit enough structure inside modern game data to provide storage savings substantially greater than the ~3.2% currently measured on Elden Ring?**

Current evidence shows:

```text
~1.58 GiB duplicate reuse
~7.49 GiB unique compressible raw data
~57.29 GiB unique incompressible raw data
```

That strongly suggests the next breakthrough, if one exists, will come from understanding the large archive/container data rather than from small adjustments to ordinary compression.

The immediate next investigation should therefore identify exactly which files account for those ~57 GiB and determine whether the large `.bdt` archives contain exploitable internal redundancy.

If they do, archive-aware virtualization may provide a path toward the desired 10–12+ GiB reduction.

If they do not, the project should document the realistic generic compression ceiling rather than sacrificing runtime performance, memory, or reliability in pursuit of an unattainable percentage.