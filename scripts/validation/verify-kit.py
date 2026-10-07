"""Verify downloaded kit integrity against its bundled build manifest (not publisher authentication)."""
import hashlib
import json
import sys
from pathlib import Path
kit = Path(sys.argv[1]).resolve()
manifest = json.loads((kit / 'build-manifest.json').read_text())
for name, expected in manifest['sha256'].items():
    assert Path(name).name == name, 'Invalid manifest path'
    assert hashlib.sha256((kit / name).read_bytes()).hexdigest() == expected, f'Kit checksum mismatch: {name}'
print(manifest['commit_SHA'])
