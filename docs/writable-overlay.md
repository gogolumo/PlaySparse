# Persistent writable overlay v1

The optional `playsparse mount BASE MOUNT --overlay OVERLAY` path uses one
`playsparse-overlay` engine in both the FUSE and WinFsp backends. The default
mount remains read-only. Writable operation is **EXPERIMENTAL**: this is regular
file and directory support, with a generated updater fixture; it is not a Steam,
Epic, anti-cheat, or physical gaming desktop compatibility claim.

## Namespace and open files

The merged namespace starts with the sealed immutable base manifest. Persistent
tombstones hide removed paths; overlay entries shadow the remaining base entries.
An overlay entry can refer to an unchanged base file, a mutable data object, or a
directory. Renaming an unchanged base tree therefore changes metadata without
copying its file payloads. Every parent must exist and be a directory.

Supported engine operations are create, open, bounded range read, partial write,
append, truncate, mkdir, unlink, empty rmdir, rename, replacement, direct-child
enumeration, permission mode changes, file flush, and volume sync. A single
atomic namespace transaction moves every descendant during a directory rename.
Replacing a directory requires an empty destination directory. File/directory
type mismatches return the corresponding error.

Open handles retain an `Arc` to the original inode. Other handles for the same
file observe writes. Rename changes that inode's visible path; replacement and
unlink do not redirect an old handle to the replacement. An unlinked handle can
still read, write, obtain metadata, and flush until it closes. Directory handles
retain identity across ancestor rename; an unlinked empty directory enumerates
as empty. New files receive monotonically allocated identifiers; root id is 1.

Each file has its own read/write lock. Concurrent reads of different files do
not take a global exclusive lock. Namespace modifications and first copy-up are
serialized; ordinary writes to files that already have data objects use the
per-file lock. Appends compute EOF under that lock.

## Whole-file copy-up

Opening an existing file for write does not copy it. Its first nonempty write or
nonzero truncate streams its entire base payload through verified range reads
into an opaque file, using buffers of at most 1 MiB. The complete copy is flushed
before its namespace reference is published. A truncate to zero creates an empty
data object without reading the base payload. Reads keep the existing 16 MiB
per-request limit; sizes and offsets are unsigned 64-bit, with a portable file
size ceiling of `i64::MAX`.

This v1 copy-up can require the full logical size of a file even for a one-byte
patch. It does not preserve compressed chunks or infer sparse extents from the
base. The measured `copy_up_bytes` is therefore essential when interpreting an
updater result. Windows data objects request `FSCTL_SET_SPARSE` when the backing
filesystem supports it, so extending a file or writing at a large offset need
not allocate the gap. This does not make copying a large base file sparse.
Block overlays and compression of mutable files are not implemented.

## On-disk format and durability

The overlay directory contains:

- `.overlay.lock`: a stable, marked lock inode, held with the OS file lock for
  the overlay's lifetime, including outstanding handles.
- `state.json`: format `playsparse-overlay`, version 1, a snapshot and its
  BLAKE3 digest. The snapshot binds the overlay to the serialized base manifest
  digest and stores the next inode id, sorted overrides, and sorted tombstones.
- `data/<16 lowercase hexadecimal inode digits>.data`: ordinary mutable file
  payloads. Logical user paths never address these files.

Snapshots are bounded by the existing 256 MiB metadata format limit. Parsing
rejects unknown fields, unsafe paths, invalid permissions or ids, duplicate
visible identities, missing/file parents, malformed base references, and digest
mismatches. Remount requires every referenced data file to exist and be regular.
The base binding prevents applying an overlay to another manifest.

Namespace publication writes a new opaque temporary metadata file, flushes it,
then atomically renames it over `state.json`. Unix directory descriptors are
also synced. Windows file flushing and atomic rename are used; equivalent
directory-fsync durability is not claimed. A failure after successful metadata
rename updates the running namespace before reporting the durability error.

Copy-up and create flush their data objects before publishing references.
Unreferenced data objects and recognized metadata temporary files are collected
under the exclusive overlay lock at the next open, after all referenced objects
have been validated. Interrupted copy-up therefore leaves the old base file
visible. Ordinary edits to an existing mutable file have ordinary filesystem
write semantics: a crash can preserve a partial write. Call flush/fsync or sync
before depending on durability. The metadata digest does not checksum mutable
file contents; end-to-end mutable data checksums are not implemented.

## Storage safety and management

Overlay storage must be separate from the immutable store. The engine rejects
absolute or traversing logical names, empty components, backslashes, drive
syntax, NUL/control characters, logical symlinks, storage symlink ancestors, and
Windows reparse points. Use a canonical storage path: macOS `/tmp` and `/var`
aliases should be resolved to their real spelling. Unix opens, publication,
and removal use retained directory descriptors with no-follow opaque file opens;
Windows holds the storage directories against external rename and checks
reparse flags. Special files, including FIFOs, fail promptly rather than
blocking a filesystem callback. Storage directories must be controlled by the
user running PlaySparse; hostile concurrent mutation by that same user is not a
separate security boundary.

`playsparse overlay status OVERLAY` validates a recognized, inactive overlay and
reports gauges. `playsparse overlay discard OVERLAY` refuses active locks,
unrecognized directories, unrelated files, and unsafe objects. It atomically
resets the recognized snapshot, then removes only recognized opaque data and
temporary files. The base binding, root, and lock inode remain. Neither command
alters the base store or original source tree. A requested discard resets the
overlay's visible changes; it is destructive to those changes.

The CLI may materialize the merged view for `overlay commit` and pack it into a
new verified store. The old base is never edited. Consult `playsparse overlay
commit --help` for the current command rather than treating commit as an
in-place operation.

## Metrics and validation

`copy_up_files`, `copy_up_bytes`, and `overlay_bytes_written` count the current
mount session. `overlay_files`, `tombstones`, `overlay_logical_bytes`,
`overlay_allocated_bytes`, and `metadata_bytes` describe the current overlay.
Status has zero session counters because it opens no filesystem session.
Physical allocation is measured through `st_blocks` on Unix and remains unknown
on Windows. These are overlay measurements, not total source-plus-store savings.

The engine tests cover both immutable layouts, ordinary operations and remount,
zero-fill and offsets beyond 8 GiB, shared and orphan handles, atomic append,
permission persistence, corrupt metadata, malformed valid-digest snapshots,
wrong bases, lock ownership, interrupted-write orphan fixtures, source/base
immutability, and symlink redirects. A Windows-only regression covers canonical
verbatim paths. These are engine unit tests; actual mounted updater results and
their commit/hardware provenance belong in runtime evidence and backend docs.

No hard links, logical symlinks, special files, persisted timestamps, extended
attributes, ACL edits, arbitrary root permission changes, block copy-up, or
real-launcher compatibility are claimed by this format.
