# Technology Stack Decision

## Main language

### Rust — preferred after M0
Pros: memory safety, strong binary tooling, high-performance I/O, good cross-platform story, C FFI for provider APIs, mature hashing/compression crates.

Risk: platform filesystem bindings still require unsafe/FFI edges and platform-specific engineering.

### C++
Best access to native Windows/filesystem APIs and existing examples, but a larger memory-safety burden for an I/O-heavy daemon.

### Go
Excellent tooling and concurrency; less ideal for low-level provider APIs and latency-sensitive FFI-heavy filesystem paths.

### Swift
Strong for a macOS-only product, but not the best foundation for a Windows-first cross-platform storage engine.

**Decision:** M0 uses Python + system tools for fast experimentation. M1+ targets Rust unless Experiment 02 exposes a blocker.

## Compression

- **Zstd:** primary baseline; strong decompression speed and configurable ratio. Independent frames provide a simple random-access building block.
- **LZ4:** candidate for hot/cache-sensitive paths where latency matters more than ratio.
- **LZMA:** useful as a high-ratio reference, but decompression/CPU characteristics are less attractive for transparent random reads.
- **Brotli:** worth testing on selected content classes, not the default generic game-storage codec.

## Hashing

- **BLAKE3:** preferred object ID/integrity hash for performance.
- **SHA-256:** compatibility/reference hash.
- **xxHash:** useful for fast non-cryptographic fingerprinting, not sufficient alone for durable object identity.

## Platform order

1. storage engine format: cross-platform;
2. Windows provider prototype;
3. Linux FUSE validation;
4. macOS provider after the core format is stable.
