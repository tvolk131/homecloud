"""Matched ARC tests with full admission and load confined to disposable VMs.
The existing 4 GiB devbox retains its allocation and remains idle throughout.
This is not a test with every byte of configured guest memory resident at once.
"""
from pathlib import Path
import argparse,json,re,subprocess,sys,time
from control_runner import Runner,GIB
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True)
p.add_argument('--cli',default=str(ROOT/'target/debug/homecloud'))
p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'))
p.add_argument('--evidence',required=True)
p.add_argument('--resume',action='store_true')
args=p.parse_args();r=Runner(args)
helper=(ROOT/'lab/memory_pressure.py').read_text()
setup="from pathlib import Path;import json;root=Path('/var/lib/homecloud/test-runs/reliability-tools');root.mkdir(exist_ok=True,mode=0o700);path=root/'memory_pressure.py';path.write_text("+repr(helper)+");path.chmod(0o600);print(json.dumps({'installed':True}))"
r.remote(setup)
OUT=Path(args.evidence);result={'rounds':[]}
def save():OUT.write_text(json.dumps(result,indent=2)+'\n')
def policy(cores,mode):
 args=['reserve-set','--memory',4,'--cores',cores,'--disk',30];args+=['--arc-auto'] if mode=='auto' else ['--arc-limit',2]
 op=r.cli(*args);deadline=time.monotonic()+180
 while time.monotonic()<deadline:
  s=r.snapshot();o=s['host_reserves']['operation'];assert o['id']==op['id'] and o['phase']!='needs_review',o
  if o['phase']=='succeeded':return s
  time.sleep(.5)
 raise TimeoutError('Reserve update timed out')
def host(mode,*values):
 p=subprocess.run(['ssh','-F',args.ssh_config,'hclab-host','sudo','-n','python3','/var/lib/homecloud/test-runs/reliability-tools/memory_pressure.py',mode,*values],capture_output=True,text=True,timeout=300);assert not p.returncode,p.stderr
 lines=p.stdout.strip().splitlines();return json.loads(lines[-1]) if lines and lines[-1].startswith('{') else p.stdout.strip()
if r.state:
 assert args.resume,'Existing run: inspect it, then use --resume'
 result['initial']=host('inspect');save()
 assert result['initial']['proof_dataset']==r.state['memory_dataset']
 ids=[e['id'] for e in r.state['fixtures']];r.wait(ids,'running');prepared=True
else:
 r.start_run();result['initial']=host('inspect');save()
 assert not result['initial']['proof_prepared']
 policy(1,'auto');ids=r.create_batch(3,memory=2);ids+=r.create_batch(1,memory=1);r.wait(ids,'running');prepared=False
print('All 11 GiB/five cores are claimed; load is confined to four disposable guests',flush=True)
try:
 if not prepared:
  prepared_state=host('prepare');prepared=True;r.state['memory_dataset']=prepared_state['proof_dataset'];r.save()
 host('register',*['hc-'+i.replace('-','') for i in ids])
 for mode in ['auto','custom']:
  policy(1,mode);host('warm');before=host('inspect');host('start-io');host('fill')
  sample={'mode':mode,'effective_cap_bytes':before['arc']['maximum_bytes'],'before':before,'samples':[]};result['rounds'].append(sample);save()
  start=time.monotonic()
  while time.monotonic()-start<120:
   point=host('inspect');point['elapsed_seconds']=round(time.monotonic()-start,2);sample['samples'].append(point);save()
   assert point['vmstat']['oom_kill']==before['vmstat']['oom_kill'] and point['pool_healthy']
   time.sleep(3)
  sample['guests']=host('guest-status');last=sample['samples'][-1]
  assert all(g['unit']=='active' and g['allocation'] is not None and g['oom_kill']==0 for g in sample['guests'].values())
  sample['allocated_guest_bytes']=sum(g['allocation']['allocated_bytes'] for g in sample['guests'].values());assert sample['allocated_guest_bytes']>=4*GIB
  sample['swap_out_bytes']=(last['vmstat']['pswpout']-before['vmstat']['pswpout'])*4096
  settling=[p for p in sample['samples'] if p['elapsed_seconds']>=90];assert len(settling)>=5
  sample['final_30s_swap_out_bytes']=(settling[-1]['vmstat']['pswpout']-settling[0]['vmstat']['pswpout'])*4096
  sample['final_psi_avg10']=max(float(v) for v in re.findall(r'avg10=([0-9.]+)',last['pressure']))
  assert sample['effective_cap_bytes']==2*GIB and last['arc']['maximum_bytes']==2*GIB
  assert sample['swap_out_bytes']<=before['memory']['MemTotal']//100 and sample['final_30s_swap_out_bytes']<=16*1024**2 and sample['final_psi_avg10']<1
  sample['passed']=True;save();host('stop-load');print(mode+' passed: '+str(sample['swap_out_bytes'])+' bytes paged out',flush=True)
 assert {k:v['allocation']['allocated_bytes'] for k,v in result['rounds'][0]['guests'].items()}=={k:v['allocation']['allocated_bytes'] for k,v in result['rounds'][1]['guests'].items()},'Working sets must match'
 result['passed']=True;save()
finally:
 if prepared:host('stop-load')
 # Explicit recorded teardown is safe on success; failed proofs retain VMs for inspection.
 if result.get('passed'):
  r.cleanup();policy(2,'custom')
  if prepared:
   assert host('inspect')['proof_dataset']==r.state['memory_dataset']
   host('cleanup')
  result['final']=host('inspect');result['original_grants_restored']=r.snapshot()['me']['grant']==r.state['original_grant'];save()
