# Game-awareness raw evidence retention

This curated replacement preserves the full raw tree from PR #13 commit
`84faf91e6c2b183fdd2d26d316d46af95407207f` in a deterministic gzip/tar bundle.
No game assets are included. PR #13 remains intact for review.

Important result JSON, build provenance, source/base inventories, observed
native Mac trees and nonempty native failure stderr remain expanded here.
Every other log and trace is retained in the bundle rather than as thousands
of separate Git entries. The inventory gives original paths, byte lengths and
SHA-256 hashes. Bundle retention in Git avoids relying on expiring CI artifacts.

From the repository root, verify the bundle SHA-256 against
`docs/evidence/bundles/game-awareness-20261005.inventory.json`, then restore the
complete original paths with:

```sh
tar -xzf docs/evidence/bundles/game-awareness-20261005.tar.gz
```

Extraction also restores historical report links to unexpanded logs. It does
not contain runtime stores or installations. Verify every member against the
inventory before using evidence. The creating audit verified all 2,581 file
hashes after reopening the final compressed bundle.

ZIP results describe projected cross-version update reuse, not full-install
compression gains. Sampling skip-compression remains explicitly experimental.
Native writable/tiered failures in this evidence remain failures; later
validator fixes require new mounted evidence and do not rewrite these results.
