"""SHA256 of final distributable packages; certificates/keys are never inputs."""
import hashlib
import sys
from pathlib import Path
root = Path(sys.argv[1])
files = sorted(p for p in root.rglob('*') if p.is_file() and p.suffix in {'.dmg', '.exe', '.deb', '.AppImage', '.gz'})
with (root / 'SHA256SUMS').open('w') as out:
    for path in files:
        digest = hashlib.file_digest(path.open('rb'), 'sha256').hexdigest()
        out.write(f'{digest}  {path.relative_to(root).as_posix()}\n')
