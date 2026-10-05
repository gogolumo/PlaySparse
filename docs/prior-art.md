# Prior-art audit (2026-10-02)

PlaySparse must not claim novelty for components that already exist. This audit defines the baseline.

| Technology | What it does | Transparent | Random access | Compression | Dedup | Virtual files / hydration | Adaptive/game-aware | Main limitation relative to PlaySparse hypothesis |
|---|---|---:|---:|---:|---:|---:|---:|---|
| Windows WOF / CompactOS / `compact.exe` | Filesystem-level transparent compression | yes | yes | XPRESS/LZX family | no | no | no | Static per-file compression; no CAS, runtime learning, tiering or cross-version chunk reuse |
| CompactGUI | UI/automation over Windows native transparent compression | yes | yes | WOF algorithms | no | no | limited heuristics | Does not create a virtual content-addressed game store |
| GameCompact | Parallel game-library optimizer using CompactOS/LZX | yes | yes | LZX | no | no | game discovery, not access learning | Strong baseline for Windows transparent compression; PlaySparse must beat it on a different frontier, not reimplement it |
| Flummox | Compresses game libraries using native btrfs/WOF and a FUSE-backed mode | yes | yes | zstd/LZX depending backend | backend-dependent | FUSE mode | samples compressibility, watches installs | Demonstrates that "compress games and keep playing" is already solved as a filesystem feature |
| Btrfs / ZFS / EROFS / SquashFS | Filesystem/image compression | yes/mostly | yes | multiple | some filesystems support dedup separately | no/on-image | no game semantics | Static storage policies; not driven by game read traces |
| WinFsp | Framework for user-mode Windows filesystems | yes | provider-defined | provider-defined | provider-defined | yes | provider-defined | Framework, not a game storage policy engine |
| Windows ProjFS | Projects a user-mode backing store as normal files/directories | yes | provider supplies file data | provider-defined | provider-defined | yes | provider-defined | Designed for high-speed backing stores; slow-remote UX belongs more naturally to Cloud Files API |
| Windows Cloud Files API | Placeholder files with hydration/dehydration policies | yes | yes | provider-defined | provider-defined | yes | policy-driven | Generic sync/hydration primitive, not a game-specific compression/runtime optimizer |
| VFS for Git | Virtualizes huge Git worktrees and downloads objects on demand | yes | yes | Git object storage | Git object reuse | yes | access-aware worktree behavior | Strong precedent for lazy materialization, but optimized for source trees/Git operations rather than game I/O |
| Oodle Kraken/Leviathan/etc. | Game-oriented lossless codecs with explicit ratio/decode-speed trade-offs | integration-level | yes | yes | no | no | developer chooses codec | Developer-integrated codec family; PlaySparse cannot claim novelty for multi-codec speed/ratio tuning alone |
| FastCDC | Fast content-defined chunking for deduplication | n/a | n/a | no | enables it | no | no | Established CDC; PlaySparse novelty cannot be "we use CDC" |
| BLAKE3 | High-speed cryptographic hash with tree structure and parallel implementations | n/a | streaming | no | enables CAS identity | no | no | Hash primitive, not a storage architecture |

## Sources

- Microsoft ProjFS overview: https://learn.microsoft.com/windows/win32/projfs/projected-file-system
- Microsoft ProjFS provider overview: https://learn.microsoft.com/windows/win32/projfs/provider-overview
- Microsoft Cloud Files hydration policies: https://learn.microsoft.com/windows/win32/cfapi/build-a-cloud-file-sync-engine
- WinFsp API: https://winfsp.dev/doc/WinFsp-API-winfsp.h/
- VFS for Git: https://github.com/microsoft/VFSForGit
- CompactGUI: https://github.com/IridiumIO/CompactGUI
- GameCompact: https://github.com/Gabarsolon/game-compact
- Flummox: https://github.com/bybrooklyn/flummox
- Oodle compression: https://www.radgametools.com/oodle.htm
- Oodle compressors: https://www.radgametools.com/oodlecompressors.htm
- FastCDC paper (USENIX ATC 2016): https://www.usenix.org/conference/atc16/technical-sessions
- FastCDC Rust implementation: https://github.com/nlfiedler/fastcdc-rs
- BLAKE3 official implementation: https://github.com/BLAKE3-team/BLAKE3

## What is *not* novel

The following are explicitly prior art and must never be presented as PlaySparse's breakthrough:

- transparent game-folder compression;
- on-demand filesystem projection;
- placeholder hydration;
- content-defined chunking;
- content-addressed storage;
- choosing among codecs with different speed/ratio trade-offs;
- caching hot data.

## Candidate differentiator

The research hypothesis worth testing is the **integration and automatic control loop**:

> Can a transparent storage runtime for an unmodified game use observed byte-range access traces to choose chunk boundaries, codec, cache representation, prefetch behavior and storage tier per region, and thereby move the space/latency/CPU Pareto frontier beyond static filesystem compression?

This is a hypothesis, not a novelty claim. A broader literature/patent review is required before any formal claim of invention.

## Game-structure research source (2026-10-05)

[Universal-modder](https://github.com/rehan-remade/universal-modder) is MIT-licensed
engine/game discovery and modding research tooling. PlaySparse reviewed current
`main` at `0f5dcdfdcd8ed420f8413815bd6647586ab894a2` and uses only an optional
read-only scan adapter plus engine/container knowledge as research hints. Modding,
injection, asset extraction, loaders and the network knowledge workflow are not
integrated. See the [audit](game-awareness-audit.md) and [notice](../THIRD_PARTY_NOTICES.md).
Engine identification, compression sampling and stable archive boundaries are
not claimed as inventions. Experiment 05 isolates whether these known ideas
improve the existing generic frontier; discovery alone has no measured codec gain.
