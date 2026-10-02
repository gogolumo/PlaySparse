# Limitations

- Compression cannot beat information entropy; already-compressed data may not shrink.
- Independent chunks trade compression ratio for bounded random-read work.
- Synthetic benchmarks do not predict savings for a specific game.
- Game launchers, patchers, anti-cheat and DRM may expect exact filesystem behavior beyond simple reads.
- Writable virtualization is substantially harder than read-only projection.
- Remote backing changes failure modes: latency, availability, bandwidth and offline use matter.
- Perceptual asset recompression can break checksums and content formats and is outside Safe mode.
- Filesystem correctness includes metadata, locking, memory mapping, sparse semantics and unusual access patterns—not just `read()`.
