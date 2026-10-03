# Verified object tiers and HTTP ranges v1

`playsparse mount BASE MOUNT --tiers CONFIG.json` opts into object resolution
through optional local and HTTP tiers. Immutable logical identity remains the
raw chunk's BLAKE3 digest. A secondary store uses its own index, codec, and
physical locator; it need not share the base's loose/pack layout or compression
choices. The base manifest and index remain sealed local metadata.

## Configuration

The strictly parsed JSON schema is versioned and rejects unknown fields:

```json
{
  "version": 1,
  "primary_cache": "promoted-chunks",
  "secondary": "secondary-store",
  "remote": {
    "base_url": "http://127.0.0.1:8080/store",
    "timeout_ms": 3000,
    "retries": 0
  },
  "promote": true
}
```

Optional paths and `remote` may be omitted or null. Relative paths resolve
against the configuration file's canonical parent. Paths must not contain parent
traversal. The example is a portable schema example, not a committed machine
configuration. Config input is bounded to 1 MiB. Promotion requires a separate
`primary_cache`; without promotion, an explicitly configured cache directory must
already exist. The timeout accepts 10..60000 ms. `retries` means additional
attempts, accepts 0..2, and defaults to zero; total attempts never exceed three.

HTTP(S) URLs must have a host and must not have userinfo, a query, a fragment, or
backslashes. Authentication headers and URL credentials are not supported.
Redirects and environment proxies are disabled. HTTPS uses normal certificate
verification; no insecure TLS bypass exists. Errors deliberately omit endpoint
URLs, credentials, headers, and response bodies.

## Resolution and verification

Resolution order is verified promoted primary cache, base primary storage,
optional secondary store, then optional HTTP. Missing objects permit moving to
the next tier. Corruption, malformed cache headers, unreadable local objects,
wrong sizes, and promoted-cache symlinks fail the read; they are not hidden by
trying another copy. Reads from any source are checked for the expected raw
length and BLAKE3
before being delivered to the range/cache engine or filesystem application.

Ordinary `Store::open` remains strict and requires primary payloads. Configured
`Store::open_with_tiers` permits missing primary payload files while validating
the seal, manifest, sorted fixed-width index, chunk references, locator rules,
logical pack coverage, and every primary pack that is actually present. The
configured secondary directory currently must open as a complete valid local
store. This is a metadata-local design; fetching a remote manifest/index is not
implemented.

A promoted chunk uses an independent raw format:

| Field | Bytes |
| --- | ---: |
| `PSPTIER1` magic | 8 |
| Raw size, little-endian u32 | 4 |
| Raw BLAKE3 digest | 32 |
| Verified raw payload | raw size |

Its filename is `<64 lowercase digest digits>.psc`. Promotion intentionally
stores raw bytes and may consume more disk than its upstream compressed object.
A temporary file is exclusively created, fully written and flushed, then
published atomically without replacing an existing name. Unix uses an anchored
hard-link publication; Windows uses same-directory `MoveFileExW` with no replace
flag. A concurrent existing publication is revalidated before reuse. The cache
must not overlap the base or secondary store; neither immutable source is ever
modified. Cache directory and object opens reject symlinks/reparse points and
special files. Directory startup walks every ancestor rather than checking a
path and then following it during canonicalization. Unix creates and opens each
component relative to the previous descriptor with no-follow flags, then keeps
the final descriptor for all object operations. Windows opens components with
reparse-aware flags and retains the ancestor handles without delete sharing.
The user must control the cache directory and its ancestors.

Base and secondary CAS directories must also remain under the user's control.
Their existing read-only storage paths follow operating-system links; object
length and BLAKE3 verification still apply to linked payloads. They are trusted
immutable inputs, rather than a filesystem confinement boundary. The stricter
no-follow rules above apply to the separate writable promotion cache.

Interrupted promotion may leave an ignored `.promote-*.tmp` file. Automatic
garbage collection and a disk-capacity/eviction policy for these promoted files
are not implemented. Per-object allocation and network buffers are bounded by
the existing 16 MiB chunk limit; the byte-bounded in-memory cache is separate.

## HTTP object ranges

The configured remote serves the payload tree corresponding to the sealed base
index. Each pack request is exactly:

```text
GET <base_url>/packs/pack-NNNN.psp
Range: bytes=<object offset>-<object last byte>
Accept-Encoding: identity
```

Loose objects use `objects/<first two digest digits>/<remaining digits>.pso`
and request their exact object extent. A response must be HTTP 206 with the
expected `Content-Range`, exact `Content-Length`, and identity/no content
encoding. Pack total size must match the length implied by the sealed index.
The reader consumes at most encoded-size-plus-one bytes and rejects truncated,
oversized, incorrectly ranged, incorrectly encoded, or corrupted objects.
Servers that ignore Range and return HTTP 200 are rejected. A large pack is
never downloaded to satisfy a single chunk read.

Transport failures, timeouts, and HTTP 5xx can use the configured bounded
additional attempts. HTTP 404, protocol failures, redirects, and integrity
failures do not trigger unbounded retries. Failed promotion is reported; a full
or unwritable cache is not silently treated as a successful promotion. No
offline guarantee exists for an object absent from all available local tiers.
Already promoted or primary/secondary-resident objects need no HTTP request.

## Metrics and evidence

Metrics include primary/secondary/remote hits, primary cache hits, payload bytes,
remote requests/ranges/errors/timeouts, integrity errors, and promotion
count/bytes. Primary and secondary byte counts describe loaded object payloads
(encoded upstream objects or raw promoted objects), excluding format headers.
`remote_bytes` includes received payload from unsuccessful attempts. Session
counters are not a persistent accounting ledger. Access trace events use the
actual `primary-local`, `secondary-local`, or `remote-http` source; in-memory
cache events remain `memory-cache`.

Deterministic unit tests use a local `TcpListener` server and cover exact pack
and loose ranges, primary and secondary preference, layout-independent
secondary identity, promotion/reopen/offline cache reads, corrupted promoted
data, 404, truncation, wrong ranges/bytes, HTTP 200, redirect rejection, both
header/body timeouts, bounded retries, offline local reads, sanitized errors,
strict configuration, sealed metadata validation, concurrent publication, and
symlink/overlap defenses. These tests require no external internet service.
The Unix ancestor-substitution regression races 1000 cache startups against a
symlink redirect to immutable secondary storage and checks that the secondary
tree remains unchanged. The initial failure reproduction is retained in
`docs/evidence/raw/tier-startup-race/baseline.json`; its provenance identifies
the uncommitted implementation tested. This race regression is Unix-specific.
Mounted tier results must separately name their executed OS, commit, commands,
and raw evidence; unit tests alone are not a mounted validation claim.
