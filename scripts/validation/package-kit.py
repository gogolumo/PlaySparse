"""Package generated validation tooling separately from native installers."""
import argparse
import hashlib
import json
import shutil
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--platform', required=True)
p.add_argument('--commit', required=True)
a = p.parse_args()
kit = Path('validation-kit')
kit.mkdir(exist_ok=False)
ext = '.exe' if a.platform.startswith('windows') else ''
for name in ('playsparse-validation', 'playsparse-fixture'):
    shutil.copy2(Path('target/release') / (name + ext), kit / (name + ext))
sidecars = list(Path('desktop/src-tauri/binaries').glob('playsparse-engine-*' + ext))
assert len(sidecars) == 1, 'Expected one host sidecar'
shutil.copy2(sidecars[0], kit / ('playsparse-engine' + ext))
shutil.copy2('scripts/validation/verify-kit.py', kit / 'verify-kit.py')
shutil.copytree('scripts/validation/windows', kit / 'validation/windows')
shutil.copytree('scripts/validation/linux', kit / 'validation/linux')
manifest = {'commit_SHA': a.commit, 'platform': a.platform, 'evidence': 'kit built in CI; physical test NOT VERIFIED',
            'sha256': {f.name: hashlib.sha256(f.read_bytes()).hexdigest() for f in kit.iterdir() if f.is_file()}}
(kit / 'build-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
