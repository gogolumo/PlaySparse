#!/usr/bin/env bash
set -euo pipefail
# Extract the matching desktop-validation-kit CI artifact first. No build toolchain needed.
kit=$(cd -- "$(dirname -- "$0")/../.." && pwd)
if [ "$#" -ne 2 ]; then echo 'Usage: validate.sh /absolute/path/to/PlaySparse /new/work/directory' >&2; exit 2; fi
commit=$(python3 "$kit/verify-kit.py" "$kit")
chmod u+x "$kit/playsparse-validation" "$kit/playsparse-fixture" "$kit/playsparse-engine"
exec "$kit/playsparse-validation" --app "$1" --work "$2" --engine "$kit/playsparse-engine" --fixture "$kit/playsparse-fixture" --commit "$commit" --evidence 'physical hardware validation'
