"""Controlled macOS signed-code probe. Never materializes commercial code or changes originals."""
import argparse
import hashlib
import json
import platform
import subprocess
import time
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--engine', type=Path, required=True)
p.add_argument('--work', type=Path, required=True)
p.add_argument('--commit', required=True)
a = p.parse_args()
assert platform.system() == 'Darwin'
a.work.mkdir(exist_ok=False)
work = a.work.resolve()
source = work / 'generated-source'
source.mkdir()
(source / 'main.c').write_text('#include <stdio.h>\nint main(void){ puts("generated native bytes"); return 0; }\n')
receipt = {'schema': 1, 'commit_SHA': a.commit, 'OS': platform.platform(), 'architecture': platform.machine(),
           'evidence_level': 'physical hardware validation', 'signing': 'ad-hoc controlled fixture; Developer ID NOT VERIFIED',
           'started_at': time.time(), 'stages': [], 'shadow': 'not implemented'}

def call(name, argv, required=True):
    try:
        result = subprocess.run([str(v) for v in argv], capture_output=True, timeout=60)
        receipt['stages'].append({'name': name, 'argv': [str(v).replace(str(work), '$WORK') for v in argv],
                                  'code': result.returncode, 'stdout': result.stdout.decode(errors='replace')[-8192:],
                                  'stderr': result.stderr.decode(errors='replace')[-8192:]})
        if required and result.returncode:
            raise RuntimeError(name + ' failed')
        return result
    except subprocess.TimeoutExpired:
        receipt['stages'].append({'name': name, 'error': 'timeout; no forced unmount'})
        raise

engine = a.engine.resolve()
mount = work / 'mount'
mount.mkdir()
backend = None
try:
    call('compile generated native fixture', ['cc', source / 'main.c', '-o', source / 'native-fixture'])
    call('ad-hoc fixture signature', ['codesign', '--force', '--sign', '-', source / 'native-fixture'])
    before = {f.name: hashlib.sha256(f.read_bytes()).hexdigest() for f in source.iterdir()}
    receipt['source_before_hashes'] = before
    call('signature on APFS', ['codesign', '--verify', '--strict', source / 'native-fixture'])
    call('native fixture on APFS', [source / 'native-fixture'])
    call('pack', [engine, 'pack', source, work / 'store'])
    call('verify', [engine, 'verify', work / 'store'])
    log = (work / 'mount.log').open('wb')
    backend = subprocess.Popen([str(engine), 'mount', str(work / 'store'), str(mount)], stdout=log, stderr=log)
    deadline = time.monotonic() + 20
    while not (mount / 'native-fixture').is_file():
        if backend.poll() is not None or time.monotonic() > deadline:
            raise RuntimeError('Mount not ready; retained evidence')
        time.sleep(.1)
    call('signature through virtual filesystem', ['codesign', '--verify', '--strict', mount / 'native-fixture'], False)
    result = call('native execution through virtual filesystem', [mount / 'native-fixture'], False)
    receipt['direct_native_execution'] = 'PASS' if result.returncode == 0 else 'FAIL'
    call('ordinary unmount', [engine, 'unmount', mount])
    backend.wait(timeout=10)
    receipt['result'] = 'PROBE COMPLETED; per-title compatibility NOT VERIFIED'
except Exception as error:
    receipt['result'] = 'FAIL'
    receipt['error'] = str(error)
finally:
    receipt['source_after_hashes'] = {f.name: hashlib.sha256(f.read_bytes()).hexdigest() for f in source.iterdir() if f.is_file()}
    receipt['source_unchanged'] = receipt.get('source_before_hashes') == receipt['source_after_hashes']
    receipt['finished_at'] = time.time()
    (work / 'native-code.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps({'result': receipt['result'], 'receipt': str(work / 'native-code.json')}, indent=2))
if receipt['result'] == 'FAIL':
    raise SystemExit(1)
