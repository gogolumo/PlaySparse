import sys, json, importlib.util, os
from pathlib import Path
repo=Path.cwd()
sys.path.insert(0,str(repo/'tools'))
from validation_common import CommandRunner,atomic_json,repository_identity,safe_new_work,digest,signals,inspect_binary_provenance
spec=importlib.util.spec_from_file_location('updates',repo/'tools/mounted-update.py');updates=importlib.util.module_from_spec(spec);spec.loader.exec_module(updates)
spec=importlib.util.spec_from_file_location('experiment',repo/'experiments/05-game-aware-packing/run_benchmark.py');experiment=importlib.util.module_from_spec(spec);spec.loader.exec_module(experiment)
work=safe_new_work(Path('/tmp/playsparse-game-aware-native-extra-01'),repo)
cli=Path('/tmp/playsparse-game-aware-target/release/playsparse');probe=cli.parent/'io-probe'
baseline=Path('/tmp/playsparse-game-aware-baseline-01/playsparse')
before=repository_identity(repo)
report={'version':1,'status':'RUNNING','repository':before,'binary_sha256':digest(cli),'scope':'L0 native macFUSE correctness; not mounted performance or real-game evidence','commands':[],'stages':{}}
report['binary_provenance']=inspect_binary_provenance(repo,{'playsparse':cli,'io-probe':probe},Path('/tmp/playsparse-game-aware-checks-02/binary-build-manifest.json'))
r=CommandRunner(repo,work,report,timeout=600)
# Reads run in deadline-bound subprocesses, never block this lifecycle owner.
read_script="""import sys,json,hashlib;from pathlib import Path

def tree(root):
 out={}
 for p in sorted(root.rglob('*')):
  name=p.relative_to(root).as_posix()
  if p.is_dir():out[name]={'kind':'directory'}
  elif p.is_file():out[name]={'kind':'file','bytes':p.stat().st_size,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
  else:raise RuntimeError(name)
 return out
expected,actual=tree(Path(sys.argv[1])),tree(Path(sys.argv[2]))
extra={k:v for k,v in actual.items() if k not in expected}
missing=[k for k in expected if k not in actual]
changed=[k for k in expected if k in actual and expected[k]!=actual[k]]
print(json.dumps({'expected':expected,'actual':actual,'extra':extra,'missing':missing,'changed':changed,'full_tree_equal':expected==actual}))
if missing or changed:raise SystemExit(1)
if len(sys.argv)<4 and extra:raise SystemExit(1)
"""
try:
 with signals():
  # Audit the retained failed writable overlays without suppressing sidecars in the original gate.
  audits=[]
  for name,path,binary in [('main',Path('/tmp/playsparse-game-aware-native-main-writable-01'),baseline),('candidate',Path('/tmp/playsparse-game-aware-native-mac-01/writable'),cli)]:
   label='writable-diff-'+name
   mount=updates.Mount(binary,path/'base',path/'mounted',path/'overlay',work/(label+'.trace.jsonl'),work,label,report['commands'])
   with mount:
    r.run([sys.executable,'-c',read_script,str(path/'expected'),str(path/'mounted'),'permit-recorded-extra-sidecars'],label+'-read')
    diff=json.loads((work/(label+'-read.stdout.log')).read_text());atomic_json(work/(label+'.json'),diff)
    if not diff['extra'] or any(not any(c.startswith('._') for c in k.split('/')) for k in diff['extra']):raise RuntimeError('unexpected extra files')
    audits.append({'baseline_or_candidate':name,'binary_sha256':digest(binary),'full_tree_equal':diff['full_tree_equal'],'extra_paths':sorted(diff['extra']),'missing':diff['missing'],'changed':diff['changed'],'original_gate_status':'FAIL'})
  report['writable_comparison']=audits
  report['stages']['writable_audit']='PASS: intended file bytes match; identical AppleDouble extras; full-tree gate remains FAIL'
  # Exercise a profile-guided ZIP store through a real native mount.
  fixture=work/'profile-fixture';fixture.mkdir();sources=experiment.generate(fixture,4)
  source=sources[1];source_before=experiment.tree_identity(source)
  profile,plan=work/'profile.json',work/'plan.json';store=work/'zip-store';mounted=work/'zip-mounted';mounted.mkdir()
  r.run([str(cli),'inspect-game',str(source),'--scanner','generic','--container-aware','--output',str(profile),'--plan-output',str(plan)],'profile-inspect')
  r.run([str(cli),'pack',str(source),str(store),'--plan',str(plan)],'profile-pack')
  r.run([str(baseline),'verify',str(store)],'profile-old-reader-verify')
  store_before=experiment.tree_identity(store)
  mount=updates.Mount(cli,store,mounted,None,work/'profile.trace.jsonl',work,'profile-mount',report['commands']);mount.ready_file=Path('content.pk3')
  with mount:
   report['profile_mount_record']=mount.record
   report['profile_devices']={'source':source.stat().st_dev,'mount':mounted.stat().st_dev}
   if source.stat().st_dev==mounted.stat().st_dev:raise RuntimeError('same device, not real mount')
   r.run([sys.executable,'-c',read_script,str(source),str(mounted)],'profile-mounted-read')
  report['profile_provider_events']=mount.events
  report['source_unchanged']=source_before==experiment.tree_identity(source)
  report['store_unchanged']=store_before==experiment.tree_identity(store)
  report['profile_clean_unmount']=not os.path.ismount(mounted) and not any(mounted.iterdir())
  if not all(report[k] for k in ['source_unchanged','store_unchanged','profile_clean_unmount']):raise RuntimeError('profile integrity/cleanup')
  report['stages']['profile_zip_mount']='PASS'
  # Independent stages run even though canonical writable validation stopped early.
  for name,script in [('adaptive','adaptive-smoke.py'),('tiers','tiered-smoke.py')]:
   print(name,flush=True)
   r.run([sys.executable,str(repo/'tools'/script),'--work',str(work/name),'--playsparse',str(cli),'--io-probe',str(probe)],name)
   payload=json.loads((work/name/'evidence/result.json').read_text());report['stages'][name]=payload['status']
   if payload['status']!='PASS':raise RuntimeError(name+' did not pass')
  report['repository_unchanged']=repository_identity(repo)==before
  report['binaries_unchanged']=digest(cli)==report['binary_sha256']
  if not report['repository_unchanged'] or not report['binaries_unchanged']:raise RuntimeError('identity changed during native validation')
  report['status']='PASS'
except BaseException as e:report.update(status='FAIL',error=str(e))
finally:atomic_json(work/'result.json',report)
print(json.dumps({'status':report['status'],'stages':report['stages'],'error':report.get('error')}),flush=True)
raise SystemExit(0 if report['status']=='PASS' else 1)
