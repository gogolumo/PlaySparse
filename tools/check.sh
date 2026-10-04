#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PYTHONPATH=.
task_check_results=$(mktemp -d "${TMPDIR:-/tmp}/playsparse-check.XXXXXX")
trap 'rm -rf "$task_check_results"' EXIT
python3 -m unittest discover -s tests -p 'test_*.py' -v
python3 experiments/02-fixed-vs-fastcdc/benchmark.py --dataset-mib 8 --chunk-kib 256 --level 1 --output "$task_check_results/02" >/dev/null
python3 experiments/03-blake3-cas/run_demo.py --output "$task_check_results/03" >/dev/null
