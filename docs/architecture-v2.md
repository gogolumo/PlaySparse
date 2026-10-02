# Architecture v2 — adaptive storage runtime hypothesis

This is the architecture to test, not a finished design.

```text
Game / application
        |
        v
Virtual FS provider
        |
        v
Range Resolver  <----- access trace profiler
        |                         |
        |                         v
        |                  Policy / optimizer
        |                   /      |      \
        v                  /       |       \
Cache Manager <---- Prefetcher  Codec   Tier placement
        |
        v
Object Resolver
   |       |       |
 NVMe     HDD     remote/NAS (later)
        |
        v
Immutable content-addressed chunks
```

## Separation of concerns

### Immutable data plane

- logical file -> chunk references;
- BLAKE3 object identity;
- compressed immutable objects;
- deterministic reconstruction.

### Mutable policy plane

- hot/warm/cold observations;
- access graph;
- prefetch scores;
- cache state;
- selected physical representation;
- tier placement.

The same logical bytes must remain stable even if policy changes.

## Central research question

Can the mutable policy plane select a better physical representation than any one static configuration for the same observed workload?

## Required progression

1. prove byte-identical CAS reconstruction;
2. expose byte-range reads without full extraction;
3. record traces;
4. establish static policy baselines;
5. add one adaptive dimension at a time;
6. retain only non-dominated policies.

## Near-term differentiator

Experiments 02 and 03 are foundations, not novel features. The first experiment that can directly test the PlaySparse differentiator is the later trace-aware adaptive chunk/codec/cache study after a working range-serving VFS exists.
