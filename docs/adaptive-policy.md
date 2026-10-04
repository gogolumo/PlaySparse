# Adaptive policy v1 — EXPERIMENTAL

`playsparse-policy` supplies a strict policy schema and bounded observations.
`playsparse-cache` changes retention decisions using those observations.
`playsparse-range` executes bounded background prefetch against verified object
sources. Omitting `--policy` retains the static byte-weighted LRU read path.
This capability does not establish a general game, launcher, or performance win.

## Configuration and commands

All fields below except optional `cache_bytes` are required. Unknown fields,
unsupported versions, unsafe
virtual paths, oversized JSON and parameters outside the listed limits fail
validation before mounting. `files` contains virtual paths, never machine paths.

```json
{
  "version": 1,
  "cache_bytes": null,
  "eviction": "decaying-hotness",
  "decay_accesses": 64,
  "prefetch": {
    "enabled": true,
    "sequential_reads": 3,
    "max_chunks": 2,
    "budget_bytes": 8388608,
    "queue_depth": 32,
    "ttl_ms": 500
  },
  "files": {}
}
```

`cache_bytes` optionally lowers the CLI cache budget; it never raises it.
Zero cache bytes disables retention and prefetch. `eviction` accepts `lru` or
`decaying-hotness`. `decay_accesses` is 1–1,000,000; sequential confidence is
2–32 reads; prefetch looks ahead 1–8 chunks; its reservation budget is at most
128 MiB; queue depth is 1–256; TTL is 1–30,000 ms. File priorities are 1–1024
and there are at most 4096. Policy input is limited to 1 MiB.

```sh
playsparse trace summarize access.jsonl
playsparse optimize access.jsonl --output policy.json
playsparse mount BASE MOUNT --cache 64M --policy policy.json --trace adaptive.jsonl
playsparse replay BASE access.jsonl --cache 64M --policy policy.json --repetitions 3 --output comparison.json
playsparse replay-one BASE access.jsonl --cache 64M --output static.json
playsparse replay-one BASE access.jsonl --cache 64M --policy policy.json --output adaptive.json
```

Mountpoint requirements remain platform-specific; see the FUSE and Windows
backend documents. Policy works with optional overlay and tier configuration.
Optimization uses a validated trace's most frequent files for initial priority
and enables prefetch when at least 20% of successful nonempty reads reach the
same sequential confidence used by the runtime. That recommendation requires
measurement on the intended workload; it does not predict a speedup.

## Decisions that change runtime behavior

File hotness increases with observations and halves after each
`decay_accesses` interval without another access. File observations and
sequential state each retain at most 4096 identities. A cached object's own
priority also increases on demand hits and decays with demand cache accesses.
Speculative lookups neither advance that aging clock nor refresh LRU recency.
These scores affect actual eviction: adaptive mode chooses the lowest decayed
priority among up to 64 least recently used candidates. Static mode evicts the
least recently used object. The bounded candidate scan controls eviction CPU;
it is not a global optimum over every resident object.

Sequential detection follows each visible file, because filesystem dispatcher
threads can change between callbacks from one application stream. It accepts
strictly advancing read windows whose start is contiguous with or overlaps the
previous window and whose end advances. Repeated windows, backward reads,
forward gaps, different files and expired state reset confidence. Kernel
read-ahead can therefore advance overlapping windows without disabling the
detector. Concurrent application streams sharing one file can still confuse
this conservative heuristic; every resulting speculative request remains
subject to the same byte, queue, time and cache bounds.
Confidence alone does not schedule a whole object after a tiny read burst.
The current forward run must cover at least the raw size of the last demanded
chunk before look-ahead begins. Overlap counts only newly covered forward bytes;
repeats, gaps, backward reads and expiry reset that coverage.

After sufficient confidence, range reads enqueue only the next configured
number of manifest chunks. One worker loads objects through the same verified
tier resolver and single-flight cache used by demand reads. Pending hashes are
deduplicated. A bounded recent-attempt history also suppresses the same hash
until its TTL after completion or an admission refusal, including failed loads.
It retains at most 4096 records; older records can be displaced under churn.
Demand reads remain free to load an object immediately. Reservations include
both queued and active requests and cannot
exceed the smaller of the prefetch budget and cache capacity. The channel has
the configured queue depth; at most one additional request is active. Expired
queued work performs no I/O. Shutdown cancels queued work and waits for the
active load, whose source timeout/retry limits still apply. TTL does not abort
an already active network request.

Runtime prefetch knows each object's manifest raw size. A queue preflight,
an atomic cache check immediately before loading, and another check after
verified loading each plan every required eviction, including entry-limit
pressure. Every victim must have decayed priority no greater than speculation's
zero priority. Planning never partially evicts objects before deciding to admit.
It considers at most 128 oldest entries and 64 required victims, using the same
64-candidate priority/LRU ordering. If that bounded search cannot prove safe
admission, prefetch conservatively skips the load. A full eight-loader demand
pipeline also skips speculation instead of making it wait for a load permit.

Concurrent demand can change retention eligibility while source I/O is active.
The final check preserves protected objects; an unused loaded object which can
no longer fit is counted as wasted. A demand joining that flight turns it into
demand admission. Demand loads continue to make progress even when every
resident object has high priority. The cache's compatibility API for unsized
speculative loaders checks all victims after loading; runtime prefetch uses the
known-size API to avoid rejected I/O in advance.

## Bounds, tracing and metrics

Retained payload bytes never exceed the selected cache budget, and the cache
also limits retained entries to 65,536. `peak_resident_bytes` records the actual
maximum after admission. These counters cover retained raw payloads. Read
outputs, up to eight single-flight loaders, compressed/decompression buffers,
metadata, trace buffers, and OS filesystem caches are separate allocations;
the CLI cache budget is not a total process-RAM limit.

`hits`, `misses` and `hit_ratio` describe demand cache lookups. Verified raw
bytes loaded and decompression/load counts include speculation. The runtime
reports prefetch requests, loaded chunks/bytes, useful hits/bytes, wasted bytes,
unconsumed resident bytes, errors, expired/dropped requests, skipped admissions
or cache/load-slot races, suppressed pending/recent duplicates, outstanding byte
reservations, peak reservations and outstanding-request high-water mark.
Before terminal retirement:

```text
prefetch_bytes_loaded = prefetch_useful_bytes
                     + prefetch_wasted_bytes
                     + prefetch_resident_unconsumed_bytes
```

Backend teardown and replay stop the worker before final metrics and retire
unconsumed objects as wasted, leaving `loaded = useful + wasted`. A demand
reader joining a speculative in-flight load claims usefulness once, and gets
the actual loaded tier rather than an invented memory-cache attribution.
Useful bytes count the whole raw object when demand first claims it, even when
the demand reads only a small range; they do not measure every consumed byte.
Cache hits report `memory-cache`; cross-source reads report `mixed`. Failed
loads without a resolved source report `unresolved`. Prefetch and copy-up are
accounted as internal work rather than fabricated application read events.

## Reproducible comparison and evidence limits

Replay accepts versioned JSONL events and replays successful base read offsets
and requested lengths in their recorded order. It rejects unknown base paths,
overlay byte versions, oversized requests and bounded-analysis limits. It
cannot reconstruct historical writable contents from an access-only trace.
Each mode reads the same sealed store and hashes all returned bytes; differing
static/adaptive hashes fail the comparison.

`replay` alternates static/adaptive order across repetitions and creates a
fresh empty userspace cache per run. It records p50/p95/p99 read latency,
demand hit ratio, loaded raw bytes and amplification, prefetch accounting,
wall time, process CPU and process peak RSS. Wall/CPU include verification
hashing and final background shutdown. The latency samples measure the range
API before the benchmark's verification hashing.

Peak RSS is a process-lifetime measurement. For comparable per-mode peaks use
`replay-one` in separate child processes, as the mounted adaptive smoke harness
does. OS page cache, kernel filesystem cache and device cache are uncontrolled;
fresh userspace cache must not be described as cold disk. Kernel cache hits do
not become userspace cache hits or range trace events.

The original mounted trial with zero prefetch requests is retained as negative
evidence. It exposed callback worker migration and overlapping kernel reads;
the detector correction has dedicated migration/overlap/repeat/gap tests.
Benchmark artifacts retain every static/adaptive run, including regressions.
Use the raw run provenance and measurements, not this algorithm description,
to decide whether a policy helps a particular workload.
The [2026-10-04 readiness comparison](evidence/production-readiness-policy.md)
retains the same-input BEFORE and AFTER trials: wasted prefetch fell, while
adaptive still had higher median p95, CPU and raw loads than same-build static.
Static remains the default.

Unit tests cover actual priority eviction and demand-only decay, variable-size
multi-victim and entry-pressure protection, pre-load and post-load demand races,
loader saturation, bounded planning/history, repeated failures, tiny sequential
bursts and chunk coverage, strict budgets, single-flight demand/prefetch races,
source attribution, conservation, expiry without I/O, queue
reservation/cancellation, sequential rejection, tracker bounds,
invalid policies, optimization and deterministic byte-equal replay. Mounted
proof and platform status are recorded separately in evidence documents.
