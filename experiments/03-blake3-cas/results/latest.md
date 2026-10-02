# Experiment 03 results

> Generated corpus only; not a commercial-game compression claim.

- Logical bytes: **2,972,540**
- Physical CAS bytes (objects + manifest): **1,184,528**
- Physical/logical ratio: **39.85%**
- Synthetic space saved: **60.15%**
- Unique object count: **9**
- Reused chunk references: **1**
- Pack: **3.779s**
- Verify: **3.735s**
- Unpack: **3.254s**
- Byte-identical reconstruction: **True**
- Source tree SHA-256: `41f4b0623466d3b964c9d159bc7ba779583f27610e5814e6960d14b6b925caf1`
- Reconstructed tree SHA-256: `41f4b0623466d3b964c9d159bc7ba779583f27610e5814e6960d14b6b925caf1`

## What this proves

The prototype can turn a directory into a BLAKE3-addressed, Zstd-compressed object store, reuse identical chunks, reconstruct every file, verify every chunk hash, and reproduce the same directory-content tree hash without full-store pre-extraction.

## What this does not prove

The corpus intentionally contains duplicate and compressible data. The reported space saving is **not** an estimate for an AAA game. The current lab implementation is Python and invokes the Zstd CLI per object, so its pack/verify times are not production performance targets.
