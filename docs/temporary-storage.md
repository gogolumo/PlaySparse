# Temporary storage and disk budgets

Full `analyze` measures fixed and CDC packing by building two verified disposable
stores. It does not extrapolate from a sample. Select its workspace explicitly:

```sh
playsparse analyze "D:\Games\Elden Ring" --temp-dir "D:\PlaySparseTemp"
playsparse analyze /path/to/game --temp-dir /Volumes/Scratch/PlaySparse
playsparse doctor --json --temp-dir /Volumes/Scratch/PlaySparse --path /path/to/output
```

Before creating any workspace directories, the CLI measures source logical bytes
and entry count, resolves the existing destination ancestor, identifies its
volume, and queries space available to the current user. The budget assumes no
compression or deduplication, includes worst-case CDC minimum chunk count and
estimated metadata/rounding allowances, and reserves 64 MiB per store. It is a
conservative estimate, not a guarantee against concurrent disk use, quotas,
source growth or unusual filesystem allocation. Integer byte counts are
authoritative; GiB in errors is derived. Insufficient space reports the path,
volume, required estimate, available bytes and a relocation suggestion. An
unmeasurable workspace fails rather than silently skipping the check.

Generic analysis removes both temporary stores before profile-guided analysis
creates its additional candidate store; `--temp-dir` also applies to that store.
Source-contained workspaces are refused before mkdir, including symlink aliases.
The original installation remains a read-only input.

`pack` stages on the **store destination volume**, then publishes by atomic
same-filesystem rename. It has a destination disk-budget check; change the STORE
argument to use a larger volume. It deliberately has no `pack --temp-dir` flag:
moving this transactional staging to another volume would require copying and
would undermine the existing atomic publication contract. No hidden system TEMP
copy is needed by generic pack.

These preflights currently cover CLI analyze/profile candidate/pack. Overlay
copy-up, overlay commit's merged tree and promotion-cache growth need additional
capacity controls. A preflight is not a disk reservation; an error after it still
leaves source content untouched and incomplete staging uncommitted.

Doctor reports the requested temporary workspace and store destination in JSON
and human output. Its tiny mount probe still uses the OS default temp directory;
`--temp-dir` selects the diagnostic location, not the probe's location.
