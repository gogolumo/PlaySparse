"""Hosted portable service simulation: explicitly excludes GUI and mounts."""
import argparse
import os
import subprocess
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('--kit', type=Path, required=True)
p.add_argument('--work', type=Path, required=True)
p.add_argument('--commit', required=True)
a = p.parse_args()
ext = '.exe' if os.name == 'nt' else ''
kit = a.kit.resolve()
for name in ('playsparse-validation', 'playsparse-engine', 'playsparse-fixture'):
    (kit / (name + ext)).chmod(0o755)
subprocess.run([str(kit / ('playsparse-validation' + ext)), '--portable', '--engine', str(kit / ('playsparse-engine' + ext)),
                '--fixture', str(kit / ('playsparse-fixture' + ext)), '--work', str(a.work), '--commit', a.commit,
                '--evidence', 'CI simulation'], check=True)
