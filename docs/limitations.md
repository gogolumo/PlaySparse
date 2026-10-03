# Limitations

- Compression cannot beat information entropy; already-compressed data may not shrink.
- Independent chunks trade compression ratio for bounded random-read work.
- Synthetic benchmarks do not predict savings for a specific game.
- Game launchers, patchers, anti-cheat and DRM may expect exact filesystem behavior beyond simple reads.
- Writable virtualization is substantially harder than read-only projection.
- Remote backing changes failure modes: latency, availability, bandwidth and offline use matter.
- Perceptual asset recompression can break checksums and content formats and is outside Safe mode.
- Filesystem correctness includes metadata, locking, memory mapping, sparse semantics and unusual access patterns—not just `read()`.


## Writable and adaptive runtime v1

- Tiny writes to a base file can copy the entire file. Copy-up bytes and allocated
  overlay bytes must be measured; writable overlays are not a storage optimization.
- Namespace metadata uses atomic digest-checked snapshots. In-place overlay data
  updates require flush/fsync; multi-file updates have no transaction or power-loss
  rollback. Retained handles and one active overlay owner are supported.
- Overlay commit stages the complete merged logical tree before packing a new
  verified store. It needs temporary disk space and is not an efficient block commit.
- Hardlinks, symlinks, Windows alternate streams, arbitrary ACL/timestamp changes,
  NTFS-specific features and broad launcher/game compatibility remain unsupported.
- Old Windows-packed manifests may mark every file read-only. Repack to capture
  current source read-only intent, or explicitly clear that attribute in the overlay.
- Trace sees resolver/callback traffic, not all application accesses served by the
  kernel page cache. Queue overflow and writer failure lose events with counters.
- Adaptive v1 uses per-file decaying observations and bounded prefetch. Dispatcher
  workers are not application streams; interleaved access can reset confidence or
  cause bounded speculation. Trace-derived priorities can make eviction worse.
- RAM cache, queue, concurrent loads and speculative bytes are bounded. Promoted
  disk chunks currently have no size cap or eviction; raw promotion can expand disk
  usage compared with encoded CAS. Local base/secondary directories are trusted
  immutable inputs; promoted cache and overlay writes use stricter path guards.
- HTTP supports verified read-only objects and exact packfile ranges, bounded
  timeouts and retries. Authentication, writable remotes and distributed coherence
  are absent. CI uses a loopback fixture, not a network/storage performance claim.
- Replay latency/CPU/RSS compare fresh PlaySparse caches. OS and device caches are
  uncontrolled. The trace records sizes/positions, not overlay content versions, so
  replay rejects writable/overlay traffic.
- Hosted WinFsp tests do not prove physical Windows desktop, games, Steam/Epic,
  DRM or anti-cheat support. macFUSE physical mounting requires an installed and
  approved driver. No driver was silently installed during this sprint.
