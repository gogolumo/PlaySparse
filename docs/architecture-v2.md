# Architecture v2 — adaptive writable runtime

The immutable v1 manifest, index, compressed objects and verify-before-publish
format remain unchanged. New mechanisms sit above and beside that data plane.

```text
application / game
        |
FUSE / WinFsp callback adapter
        |
shared merged namespace
   |                 |
persistent overlay   immutable RangeResolver
                     |       |          |
                 byte cache  trace      policy tracker
                     |                  |
                     +----- bounded prefetch worker
                     |
             verified Object Resolver
                |          |           |
          primary local  secondary    HTTP(S)
                ^          |           |
                +---- verified atomic promotion
```

`--overlay` selects a shared copy-on-write engine. Namespace snapshots bind to
the base manifest digest and publish atomically; data-file handles retain inode
identity across rename/unlink. Ordinary base reads continue through the bounded
range resolver. First modification copies one complete base file in bounded
buffers, except truncate-to-zero. This initial design measures copy-up cost and
allows a later block-overlay implementation without changing callback adapters.

`--trace` attaches a bounded asynchronous JSONL recorder. Callback threads never
write the trace file themselves; overflow drops events with visible counters.
`--policy` supplies strict versioned parameters and initial priorities. Actual
read observations decay hotness, change cache eviction and predict bounded
forward reads. One background worker shares verification, byte limits and
single-flight loads with demand reads. The kernel's page cache and read-ahead
remain outside PlaySparse's policy and trace view.

`--tiers` configures sources by immutable BLAKE3 identity. A metadata-only base
can resolve objects from a full secondary store or exact HTTP packfile ranges.
Verified promotions go to a separate disk cache, never into either immutable
store. A corrupt existing source is an error; it is not silently hidden by
another tier. Promotion currently stores raw chunks with a self-describing
header, so its disk cost differs from the compressed base.

There is no codec switching, writable distributed storage, learned model,
access graph, background tier eviction or automatic physical placement service.
The research question remains: can a policy improve the space/latency/CPU
frontier on the same workload? An implementation is not evidence of improvement.
Static/adaptive replay preserves identical request order, data hashes and cache
budgets, including negative results and uncontrolled OS/device cache state.

See [overlay](writable-overlay.md), [trace](access-tracing.md),
[policy](adaptive-policy.md), [tiers](tiered-storage.md) and
[sprint evidence](evidence/adaptive-writable-runtime.md). The last document
separates Linux mounts, hosted Windows execution and unavailable physical gates.

## Offline game-awareness layer

`playsparse-game` sits before the immutable data plane: optional bounded scanner
→ normalized GameProfile → measured probes → versioned PackingPlan → existing
pack transaction. Profile/plan sidecars remain outside the store. Pack recomputes
source identity, decisions and ZIP offsets before writing, then verifies exact
streamed bytes against the plan. Original ZIP records can reset CDC; only the
explicit negative-result compression-skip experiment bypasses Zstd for measured
candidates. Ordinary v1 readers need no engine metadata or scanner. Runtime
policy and startup priors are unchanged. See [game awareness](game-awareness.md).
