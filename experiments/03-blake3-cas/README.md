# Experiment 03 — BLAKE3 content-addressed compressed store

## Goal

Build the first actually usable PlaySparse storage primitive:

```text
source directory
    ↓
content-defined chunks
    ↓
BLAKE3 IDs
    ↓
Zstd object store
    ↓
manifest
    ↓
verified byte-identical reconstruction
```

## Properties implemented

- BLAKE3-256 object identity;
- Zstd-compressed immutable objects;
- cross-file chunk reuse;
- atomic publication of new objects;
- versioned manifest (`version: 0`);
- per-object BLAKE3 verification on read;
- per-file SHA-256 verification after reconstruction;
- whole-tree SHA-256 equality in the experiment;
- no modification of the source directory.

## Run

```bash
PYTHONPATH=../.. python3 run_demo.py
```

## Important limitations

This is still a lab implementation. It uses a small dependency-free Python BLAKE3 reference implementation that is tested against official vectors and the `zstd` CLI. Production PlaySparse should move hashing/compression in-process in Rust using the official BLAKE3 crate and a vetted Zstd binding.
