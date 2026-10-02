#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PYTHONPATH=.
python3 -m unittest -v tests/test_lab.py
python3 experiments/02-fixed-vs-fastcdc/benchmark.py --dataset-mib 8 --chunk-kib 256 --level 1 >/dev/null
python3 experiments/03-blake3-cas/run_demo.py >/dev/null
