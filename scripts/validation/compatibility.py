"""Profile-driven generated native workloads on a normal filesystem; never commercial/VFS certification."""
import argparse
import datetime
import hashlib
import json
import platform
import shutil
import subprocess
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--profiles', type=Path, required=True)
p.add_argument('--fixture', type=Path, required=True, help='Explicit generated playsparse-fixture binary; never discovered from a profile')
p.add_argument('--work', type=Path, required=True)
p.add_argument('--commit', required=True)
p.add_argument('--version', default='0.1.0')
a = p.parse_args()
assert len(a.commit) == 40 and all(c in '0123456789abcdefABCDEF' for c in a.commit)
a.work.mkdir(exist_ok=False)
fixture = a.fixture.resolve(strict=True)
fields = {'status', 'game_identifier', 'platform', 'os', 'architecture', 'launch_target', 'launcher_type',
          'filesystem_behaviour', 'case_sensitivity', 'anti_cheat', 'native_code', 'overlay_behaviour',
          'limitations', 'tested_version', 'test_date', 'evidence'}
records = []
try:
    profiles = sorted(a.profiles.glob('*.json'))
    assert 1 <= len(profiles) <= 128, 'Require 1..128 bounded profiles'
    for path in profiles:
        assert not path.is_symlink() and path.stat().st_size <= 65536, 'Profile must be bounded regular data'
        record = json.loads(path.read_text())
        assert set(record) == fields, 'Unexpected/missing profile fields'
        identity = record['game_identifier']
        assert identity and all(c.isascii() and (c.isalnum() or c == '-') for c in identity), 'Unsafe profile identity'
        target = record['launch_target']
        assert target and not Path(target).is_absolute() and all(v not in target for v in ('\\', ':', '\x00'))
        assert all(part not in ('..', '.') for part in target.split('/')), 'Profile target traversal'
        mode = record['launcher_type']
        assert mode in {'plain', 'launcher', 'multiple', 'case', 'save', 'missing', 'crash'}, 'Profiles cannot supply scripts or commands'
        root = a.work / identity
        source = root / 'source'
        runtime = root / 'runtime'
        source.mkdir(parents=True)
        runtime.mkdir()
        binary = runtime / target
        binary.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(fixture, source / 'generated-fixture')
        before = hashlib.sha256((source / 'generated-fixture').read_bytes()).hexdigest()
        status = 'Tested'
        detail = None
        if mode == 'missing':
            assert not binary.exists()
            status = 'Broken'
            detail = 'Expected absent target; no executable launched'
        else:
            shutil.copy2(fixture, binary)
            if mode == 'case':
                (runtime / 'Case').mkdir()
                (runtime / 'Case/Data.txt').write_bytes(b'case-sensitive fixture')
                (runtime / 'Case/data.txt').write_bytes(b'different case fixture')
                if (runtime / 'Case/Data.txt').read_bytes() != b'case-sensitive fixture':
                    status = 'Unsupported'
                    detail = 'Host source volume cannot represent distinct case paths; case translation NOT VERIFIED'
            if status != 'Unsupported':
                result = subprocess.run([str(binary.resolve()), mode], cwd=runtime, capture_output=True, timeout=15)
                expected = 42 if mode == 'crash' else 0
                assert result.returncode == expected, f'{identity}: unexpected exit {result.returncode}'
                if mode == 'crash':
                    status = 'Broken'
                    detail = 'Expected generated crash exit 42 observed'
                if mode == 'save':
                    assert (runtime / 'fixture-save.txt').read_bytes() == b'isolated save'
                    assert not (source / 'fixture-save.txt').exists()
        after = hashlib.sha256((source / 'generated-fixture').read_bytes()).hexdigest()
        assert before == after, 'Generated source changed'
        record.update(status=status, os=platform.system().lower(), architecture=platform.machine(), tested_version=a.version,
                      test_date=datetime.date.today().isoformat(), evidence=[{'level': 'CI simulation', 'receipt': 'compat-synthetic.json', 'commit': a.commit}])
        record['limitations'].append('Normal-filesystem generated workload only. Mount, GUI and commercial compatibility NOT VERIFIED.')
        if detail:
            record['limitations'].append(detail)
        records.append({'record': record, 'source_before_hash': before, 'source_after_hash': after, 'pass_fail': 'PASS'})
        print(f'PASS {identity}: {status}')
except Exception as error:
    (a.work / 'compat-synthetic.json').write_text(json.dumps({'schema': 1, 'evidence_level': 'CI simulation',
        'commit_SHA': a.commit, 'records': records, 'pass_fail': 'FAIL', 'error': str(error)[:4096]}, indent=2) + '\n')
    raise
receipt = {'schema': 1, 'evidence_level': 'CI simulation', 'commit_SHA': a.commit, 'records': records,
           'commercial_game_compatibility': 'NOT VERIFIED', 'pass_fail': 'PASS'}
(a.work / 'compat-synthetic.json').write_text(json.dumps(receipt, indent=2) + '\n')
