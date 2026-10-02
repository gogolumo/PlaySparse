# Parser and range fuzzing

Optional hardening; property and mounted correctness tests are the MVP gate.
With nightly and cargo-fuzz installed:

```
cargo +nightly fuzz run manifest -- -max_len=65536 -max_total_time=60
cargo +nightly fuzz run index -- -max_len=65536 -max_total_time=60
cargo +nightly fuzz run range -- -max_len=4096 -max_total_time=60
```

Run from the repository root. Fuzzer inputs/artifacts stay ignored. These targets
do not imply a completed long-running security fuzz campaign.
