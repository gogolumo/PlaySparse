# Access tracing v1

`--trace` records resolver and overlay activity as versioned JSON Lines. It works
with read-only mounts and with the optional overlay, adaptive policy and storage
tiers. Tracing is best-effort telemetry: dropped events and writer errors are
reported separately from filesystem results.

```sh
playsparse mount BASE MOUNT --cache 64M --trace access.jsonl
playsparse mount BASE MOUNT --overlay OVERLAY --tiers tiers.json --policy policy.json --trace updated.jsonl
playsparse trace summarize access.jsonl
playsparse optimize access.jsonl --output policy.json
```

The destination must not exist and its parent directory must exist. Exclusive
creation prevents overwriting previous evidence; destination errors fail startup.
Unmount normally before summarizing. The CLI drains the writer after the mount
returns and emits final `trace_closed` counters.

## Event schema and coverage

Each line is one `playsparse_trace::Event` with these required fields. Unknown
fields and unsupported versions fail summary validation.

| Field | Meaning |
| --- | --- |
| `version` | Schema version, currently `1`. |
| `session` | Writer session identifier derived from process ID and a startup nonce. |
| `ts_ns` | Monotonic nanoseconds since the writer started. |
| `op`, `path` | Operation and relative virtual path; an empty path denotes the root. |
| `offset`, `requested`, `returned` | Unsigned 64-bit byte values for data operations. |
| `latency_ns` | Time spent in that resolver or overlay operation. |
| `cache` | `hit`, `miss`, `mixed`, or `bypass`. |
| `source` | Verified data source or operation source. |
| `worker` | Rust thread identifier for the producer. |
| `success` | Whether that operation returned successfully. |

Data sources include `overlay`, `memory-cache`, `primary-local`, `secondary-local`
and `remote-http`. A range spanning different sources uses `mixed`. A failed read
with no verified source uses `unresolved`. Source labels do not contain remote
URLs, credentials or file contents.

The common engines record reads, writes, create, mkdir, unlink, rmdir, rename,
truncate and flush. Open, stat, directory enumeration and mode changes have no
separate v1 events. Rename records its source path; v1 has no destination field.
Byte fields for metadata operations are placeholders rather than metadata sizes;
truncate uses `offset` for the new length.

An unchanged base file read through the overlay keeps its current virtual name in
the trace. Internal copy-up reads and speculative prefetch loads do not create
application read events; their I/O remains visible in overlay, cache and tier
metrics. Windows can split a large callback into bounded resolver reads, so an
application request need not correspond to one event.

Reads satisfied by the OS page cache can produce no backend event. Executable
loads and mmap faults are recorded when they reach the backend. These traces
therefore describe runtime engine activity, not every application syscall or
every byte consumed by the application.

## Bounded writer and shutdown

Callbacks submit small event records to a 4096-entry queue with `try_send`.
One background worker performs JSON serialization and buffered file writes.
A full queue drops the event instead of waiting for the writer. Paths longer
than 4096 bytes, or operation/cache/source labels longer than 32 bytes, are also
dropped. Producer callbacks still allocate their bounded event strings.

A write failure increments `writer_errors` and puts the worker into drop mode.
Filesystem read/write success remains independent of tracing success. Shutdown
closes submissions, waits for active producers, drains queued events, joins the
worker and flushes its buffer. Both filesystem backends stop prefetch before
capturing final cache and tier counters.

`events_seen`, `events_written`, `events_dropped`, `queue_high_watermark` and
`writer_errors` quantify trace completeness. After all producers have ended and
shutdown has finished, `events_seen = events_written + events_dropped`; queue
high-watermark never exceeds 4096. Earlier unmount snapshots can still contain
pending events, so use `trace_closed` for final writer accounting.

`events_written` counts successful serialization into the writer, including its
buffer. Buffer flush is not an fsync durability guarantee. A crash or sink error
can leave an incomplete final line; summary rejects malformed input instead of
silently treating it as complete evidence.

## Summary interpretation and limits

The analyzer accepts at most 2,000,000 events and 256 KiB per line. It limits
distinct file paths, exact read ranges, sessions and `(session, worker, path)`
streams to 100,000 each, and operation and source labels to 64 each. Retained
string keys have a combined 32 MiB budget in addition to those count limits.
Imported events enforce the same 4096-byte paths and 32-byte categories as the
writer, plus 64-byte session/worker identifiers. Read/write byte totals reject
`u64` overflow. Optimize and replay share the bounded event deserializer.
It rejects inputs exceeding those bounds. Exact
latencies are retained and sorted within the event bound.

The JSON summary reports operation counts, returned read/write bytes, source
counts, the 20 hottest files and the 20 hottest exact read ranges. File heat
counts all recorded operations. Range heat and reread ratio use exact
`(path, offset, requested)` equality, not overlapping-byte analysis.

Sequential percentage counts reads beginning at the previous returned end for
the same `(session, worker, path)`, divided by all read events. Failed and EOF
read events remain in that denominator. Dispatcher thread changes can split
an application stream. This diagnostic differs from adaptive policy's runtime
sequential detector; see [adaptive-policy.md](adaptive-policy.md).

Cache ratios count read events: `mixed` counts as a miss and `bypass` is excluded.
They differ from per-object cache metrics. Latency p50/p95/p99 include all
recorded operations, including failures; they are not application syscall or
read-only percentiles. Dropping events can bias every summary statistic.

Schema, concurrent shutdown, queue saturation, sink failure, exclusive creation
and bounded analysis are covered by trace tests. The mounted updater and tier
smoke harness exercise tracing through real filesystem I/O. Platform evidence
and unsupported cases remain documented in [limitations.md](limitations.md);
this document does not establish physical Windows or macFUSE validation.
