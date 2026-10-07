"""Hosted driver-backed service acceptance, explicitly excluding GUI/physical proof."""
import argparse
import json
import os
import subprocess
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('--kit', type=Path, required=True)
p.add_argument('--work', type=Path, required=True)
a = p.parse_args()
kit = a.kit.resolve()
manifest = json.loads((kit / 'build-manifest.json').read_text())
ext = '.exe' if os.name == 'nt' else ''
subprocess.run([str(kit / ('playsparse-validation' + ext)), '--service-only', '--engine', str(kit / ('playsparse-engine' + ext)),
                '--fixture', str(kit / ('playsparse-fixture' + ext)), '--work', str(a.work), '--commit', manifest['commit_SHA'],
                '--evidence', 'hosted native test'], check=True)
