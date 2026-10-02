# Feasibility / Reality Check

## Can a 100 GB game become 10 GB and behave identically?

### 1. Lossless, local-only
Not as a universal guarantee. If assets are already compressed or close to their entropy limit, another lossless layer may save little or can even add overhead. A 10× ratio is possible only for unusually redundant/compressible datasets, not arbitrary AAA installations.

### 2. Lossy, local-only
Potentially much larger savings are possible by reducing texture, audio or video quality, but the result is no longer byte-identical and can break integrity checks, patching or assumptions inside game archives. This must remain opt-in and format-aware.

### 3. Local + cache
A smaller persistent compressed store plus a bounded uncompressed cache is realistic. Physical use becomes `compressed store + cache`, and performance depends on cache hit rate and decompression latency.

### 4. Local + secondary/remote storage
Large local savings become much more plausible because not all source bytes must reside on the primary SSD. This shifts the problem from compression to tiered storage and data availability.

### 5. Asset streaming
Technically powerful when the data access pattern is predictable. Startup assets can be pinned while cold assets are recalled on demand. Network/secondary-storage latency becomes a first-class constraint.

### 6. Developer-integrated solution
This has the highest ceiling because the developer knows semantic asset boundaries, optional content and quality tiers. A generic post-install tool has less information and must preserve compatibility.

## Best achievable version

Build a transparent, read-only storage layer that:

1. analyzes files without modifying the source;
2. chunks and hashes data;
3. deduplicates identical chunks;
4. compresses each stored chunk independently;
5. exposes a normal file tree through a provider/filesystem;
6. decompresses only the chunks needed for requested byte ranges;
7. caches hot chunks;
8. optionally moves cold chunks to secondary/remote backing later.

This is useful even when compression alone is modest because it creates a platform for deduplication, tiering and streaming.
