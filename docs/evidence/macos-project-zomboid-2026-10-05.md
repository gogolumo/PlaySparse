# PlaySparse — macOS Project Zomboid Hybrid Runtime Validation

## Purpose

This document records an **L2 — local real game** validation of PlaySparse using a legally installed copy of **Project Zomboid** on native Apple Silicon macOS hardware.

The test was intentionally scoped to filesystem/runtime compatibility and storage footprint. No copyrighted game payloads are committed to this repository.

The major result is:

> **Project Zomboid successfully launched, created a new character and world, entered live gameplay, streamed world assets, accepted normal interaction, saved game state, and exited normally from a PlaySparse hybrid runtime on macOS while reducing the effective allocated installation footprint by 42.75%.**

This is strong evidence for the tested title/configuration only. It is **not** a claim of universal macOS game compatibility.

---

## 1. Evidence level and scope

Evidence level:

```text
L2 — local real game
```

Validated on one physical Mac, one owned Project Zomboid installation, and one PlaySparse runtime configuration.

Validated:

- real commercial game tree analyzed, packed and verified;
- native macFUSE mount on Apple Silicon;
- byte-exact reads from the compressed store;
- hybrid APFS compatibility layer for native code and selected path-sensitive resources;
- Project Zomboid launch;
- Steam API initialization from the owned local Steam installation;
- main menu;
- new-character flow;
- new-world creation;
- world loading;
- live single-player gameplay;
- movement and ordinary world interaction;
- entering buildings / opening doors (manual user attestation);
- save activity;
- normal game-thread exit.

Not established:

- universal compatibility across macOS games;
- multiplayer compatibility;
- anti-cheat compatibility;
- DRM compatibility beyond the tested owned local launch path;
- cold-start performance vs the original install;
- a general compression breakthrough;
- L3 cross-game replication.

---

## 2. Test environment

```text
Host: native Apple Silicon Mac
CPU: Apple M4
RAM: 16 GiB
OS: macOS 26.6.2 (25G83)
Architecture: arm64
macFUSE: 5.4.0
Game: Project Zomboid
Game version observed in runtime log: 42.21.0
Game revision observed in runtime log: 4a0e9546ec
Steam App ID: 108600
```

PlaySparse code revision used for the experiment:

```text
f96d76748cca48b0a4baae5b275c05014acc6add
```

Subsequent repository commits before this report were documentation-only and did not change the runtime code used for the session.

---

## 3. Source installation

The owned local installation contained:

```text
48,091 files
logical bytes: 10,529,753,899
allocated bytes: 10,407,190,528
allocated MiB: 10,163.27
```

The application bundle uses:

```text
Project Zomboid.app/Contents/MacOS/JavaAppLauncher
```

The launcher is a universal Mach-O with Apple Silicon and x86_64 support.

No game payloads are included in this repository.

---

## 4. Pack and verification

The tested PlaySparse store used the practical CDC/Zstd configuration selected during the experiment:

```text
chunker: CDC
average chunk target: 1 MiB
Zstd level: 6
```

Observed pack result:

```text
logical bytes:            10,529,753,899
physical bytes:            5,125,650,458
object bytes:              5,108,185,904
metadata bytes:               17,464,554
allocated bytes:           5,188,616,192
files:                            48,091
unique objects:                   39,711
reused chunks:                    12,592
verified before publish:            true
```

Independent `playsparse verify` result:

```text
ok: true
checked files:   48,091
checked bytes:   10,529,753,899
checked objects: 39,711
```

Store-only allocated reduction:

```text
Original: 10,163.27 MiB
Store:     4,948.25 MiB
Reduction: 51.31%
```

That number is **store-only** and is not the effective runtime footprint once the macOS compatibility layer is included.

---

## 5. macOS signed-code boundary discovered

Direct execution exposed a macOS/macFUSE compatibility boundary for runtime-loaded signed native code.

For `libsteam_api.dylib`:

- original APFS file and mounted file had identical SHA-256;
- `codesign --verify --strict` accepted the file on the PlaySparse mount;
- direct `dlopen` of the original APFS file passed;
- direct `dlopen` of the same bytes from the PlaySparse/macFUSE mount failed with `code signature invalid ... errno=1`;
- copying the exact mounted bytes back to APFS made `dlopen` pass again.

SHA-256:

```text
b2260d2b2ff6ac8d2d10770047967ceb18022fc5c27f94e3246bd7d2a1da82c0
```

This supports the narrow conclusion:

> On this macOS 26.6.2 / macFUSE 5.4.0 setup, valid signed native code that fails when runtime-loaded directly from the PlaySparse FUSE mount can load when the exact bytes are materialized on APFS.

PlaySparse does not disable SIP, Gatekeeper, code signing, DRM or other platform security mechanisms.

---

## 6. Hybrid runtime design used for the proof

The successful proof used a small APFS shadow runtime.

Conceptually:

```text
PlaySparse compressed store
        |
        +-- large game assets ------------> macFUSE / virtual reads
        |
        +-- signed/native runtime --------> APFS materialized shadow
        |
        +-- fonts + Lua ------------------> APFS compatibility shadow
```

The initial proof showed two macOS compatibility issues:

1. runtime-loaded signed Mach-O / dylib code required APFS materialization on this host;
2. Project Zomboid performs path handling that can lower-case paths, while the current POSIX PlaySparse namespace is case-sensitive.

For the successful proof, native/JRE files plus selected path-sensitive `fonts` and `lua` content were materialized on APFS. Large media subtrees such as maps, textures, music, UI assets and other resources continued to resolve through PlaySparse.

This is a prototype compatibility layer, not yet an automatic `playsparse launch` feature.

---

## 7. Effective storage footprint

Measured allocated footprint after constructing the successful hybrid runtime:

| Representation | Allocated |
|---|---:|
| Original installation | 10,163.27 MiB |
| PlaySparse store | 4,948.25 MiB |
| APFS hybrid shadow | 869.80 MiB |
| **Effective PlaySparse runtime** | **5,818.05 MiB** |

Result:

```text
Saved:           4,345.23 MiB
Reduction:          42.75%
Effective ratio:     0.572x
```

This 42.75% figure is the correct measured storage claim for the tested hybrid prototype.

The current shadow is deliberately simple and duplicates data that a production content-addressed native cache could avoid. Therefore this report does not claim 42.75% is the maximum achievable reduction.

---

## 8. Clean gameplay validation

A fresh PlaySparse mount was created with a fresh trace target before the final gameplay run.

The runtime log records:

- native arm64 JVM startup;
- OpenGL initialization on Apple M4;
- FMOD initialization;
- Steam API initialization;
- UI and texture-pack loading;
- scripts and Lua loading;
- `Game booting`;
- main-screen exit;
- loading-queue exit;
- world-generation initialization;
- map metadata loading;
- tile-definition loading;
- physics initialization;
- world-streamer initialization;
- game save during world setup;
- `GameLoadingState` exit;
- `1 players found`;
- `Game Mode: Apocalypse`;
- live gameplay;
- save activity on exit;
- `GameThread exited`.

Manual interaction during the same session additionally confirmed:

```text
PASS — created a character
PASS — entered the generated world
PASS — walked through the world
PASS — opened doors
PASS — entered buildings
PASS — ordinary gameplay remained responsive
PASS — exited normally
```

The runtime log also contains warnings/errors that are present in the original direct launch or are non-fatal game-content warnings. They did not prevent world creation or gameplay in this validation.

---

## 9. Development trace

A longer development-session trace captured while the hybrid design was being debugged produced:

```text
events:                    156,566
bad lines:                       0
sessions:                        1
successful events:        156,566
failed events:                   0
read operations:           156,566
write operations:                0
read bytes:           2,411,273,285
read GiB:                    2.246
unique files:               11,087
unique ranges:             141,154
sequential reads:             85.60%
reread ratio:                  9.84%
cache hit ratio:              92.98%
cache miss ratio:              7.02%
latency p50:                1,125 ns
latency p95:               16,125 ns
latency p99:              477,833 ns
```

Source distribution:

```text
memory-cache:   145,578 events
primary-local:   10,167 events
mixed:              821 events
```

This trace is retained as **development-session evidence**, not as a clean gameplay-only benchmark, because diagnostic/materialization reads occurred during the same trace session.

The built-in trace summarizer rejected this trace with:

```text
trace analysis exceeds identity limits
```

The trace itself was valid; the development trace contained 141,154 unique ranges, exceeding the current 100,000-identity analysis bound.

---

## 10. Evidence interpretation

This experiment establishes:

> A legally installed Project Zomboid build can run real single-player gameplay on native Apple Silicon macOS while a substantial portion of its installation is served from a compressed PlaySparse store, using an APFS compatibility shadow for resources that current macOS/FUSE semantics prevent from being served directly.

It does **not** establish:

> Every game will run, all game files can remain directly on FUSE, or PlaySparse universally beats native storage/compression.

The result is still important because it advances the project from launch-only evidence to a real gameplay workload with measured effective storage savings.

---

## 11. Engineering follow-ups

The proof identifies concrete product work:

1. automatic APFS native-code materialization keyed by content hash;
2. macOS-compatible case-insensitive directory lookup with deterministic collision handling;
3. automatic hybrid shadow construction;
4. large-trace summarization beyond the current 100k identity limit without unbounded memory use;
5. classify and remove noisy FUSE reply errors during teardown;
6. add a first-class launch workflow only after the compatibility layer is tested and documented;
7. repeat on unrelated games/hardware before any L3 or broad compatibility claim.

The target product flow is conceptually:

```text
playsparse pack <game> <store>
playsparse launch <store>
```

The second command does not exist at the time of this report.

---

## Conclusion

**PASS — L2 local real-game gameplay evidence.**

Project Zomboid progressed through launch, world creation, loading and live single-player gameplay from a PlaySparse hybrid runtime on macOS. The measured effective allocated footprint was reduced from **10,163.27 MiB to 5,818.05 MiB**, saving **4,345.23 MiB (42.75%)**.

The result is intentionally scoped to this title, host and hybrid runtime design. It is evidence of working technology, not a universal compatibility or compression-breakthrough claim.
