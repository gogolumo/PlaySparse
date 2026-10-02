#!/usr/bin/env bash
set -euo pipefail
python3 -m py_compile experiments/01-zstd-random-access/benchmark.py
python3 experiments/01-zstd-random-access/benchmark.py --dataset-mib 16 --random-reads 20
