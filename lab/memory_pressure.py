"""Bounded, lab-only ARC pressure proof; all temporary resources have explicit markers."""
from pathlib import Path
import json,os,re,socket,sqlite3,subprocess,sys,time,uuid
assert os.geteuid()==0 and socket.gethostname()=='resolvr-box'
sys.path.insert(0,'/usr/local/libexec/homecloud');import arc_policy
STATE=Path('/var/lib/homecloud/host/memory-pressure-proof.json');GIB=1024**3

def run(*args,timeout=45,check=True):return subprocess.run(args,check=check,capture_output=True,text=True,timeout=timeout)
def load():return json.loads(STATE.read_text())
def save(s):
 temporary=STATE.with_suffix('.tmp')
 with temporary.open('w') as f:json.dump(s,f);f.flush();os.fsync(f.fileno())
 temporary.chmod(0o600);temporary.replace(STATE)
def guest(name):
 assert re.fullmatch(r'hc-[a-f0-9]{32}',name)
 v=json.loads(run('incus','query','/1.0/instances/'+name).stdout)
 assert v['config']['user.homecloud.id'].replace('-','')==name[3:]
 assert name in load().get('proof_vms',[]),'Only registered disposable proof VMs may receive load'
 with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
  managed=json.loads(db.execute('SELECT record FROM vms WHERE id=?',[v['config']['user.homecloud.id']]).fetchone()[0])
 assert managed['spec']['name'].startswith('hc-proof-') and not managed['spec'].get('applications'),'Permanent workloads are excluded'
 return v

def inspect():
 mem={line.split(':')[0]:int(line.split()[1])*1024 for line in Path('/proc/meminfo').read_text().splitlines() if line.split(':')[0] in ['MemTotal','MemAvailable','SwapTotal','SwapFree']}
 vmstat={k:int(v) for k,v in map(str.split,Path('/proc/vmstat').read_text().splitlines()) if k in ['oom_kill','pswpin','pswpout']}
 result={'memory':mem,'vmstat':vmstat,'arc':arc_policy.observe(),'pressure':Path('/proc/pressure/memory').read_text().strip(),'pool_healthy':'healthy' in run('zpool','status','-x','hclab').stdout,'proof_prepared':STATE.exists(),'proof_dataset':load()['dataset'] if STATE.exists() else None,'scan':next((line.strip() for line in run('zpool','status','hclab').stdout.splitlines() if line.strip().startswith('scan:')),'not reported')}
 return result

mode=sys.argv[1]
if mode=='inspect':print(json.dumps(inspect()))
elif mode=='prepare':
 assert not STATE.exists(),'A previous proof needs cleanup'
 token=uuid.uuid4().hex;dataset='hclab/homecloud-memory-proof-'+token;mount='/var/lib/homecloud/memory-proof-'+token
 run('zfs','create','-o','quota=64G','-o','compression=off','-o','dedup=off','-o','atime=off','-o','primarycache=all','-o','homecloud:proof=memory-policy','-o','mountpoint='+mount,dataset)
 save({'dataset':dataset,'mount':mount,'proof_vms':[],'scrub_started':False})
 print('Writing a bounded 4 GiB cache working set',flush=True)
 run('dd','if=/dev/zero','of='+mount+'/warm','bs=16M','count=256','conv=fdatasync','status=none',timeout=240)
 print(json.dumps(inspect()))
elif mode=='register':
 s=load();names=sys.argv[2:];assert all(re.fullmatch(r'hc-[a-f0-9]{32}',n) for n in names)
 for name in names:
  v=json.loads(run('incus','query','/1.0/instances/'+name).stdout)
  # Verify the CLI-created VM name from its canonical managed record.
  assert v['config'].get('user.homecloud.id').replace('-','')==name[3:]
 s['proof_vms']=names;save(s);print('Registered proof VM identifiers')
elif mode=='warm':
 s=load();run('dd','if='+s['mount']+'/warm','of=/dev/null','bs=16M','count=256','status=none',timeout=180);print(json.dumps(inspect()))
elif mode=='start-io':
 s=load();status=run('zpool','status','hclab').stdout
 if 'scrub in progress' not in status:
  run('zpool','scrub','hclab');s['scrub_started']=True;save(s)
 code="import os,time; p="+repr(s['mount']+'/writes')+"; end=time.monotonic()+180; data=b'x'*(16*1024*1024)\nwith open(p,'wb',buffering=0) as f:\n while time.monotonic()<end:\n  f.seek(0)\n  for i in range(128):f.write(data)\n  os.fsync(f.fileno())"
 run('systemd-run','--unit=homecloud-memory-io-proof','--collect','--property=RuntimeMaxSec=200','/usr/bin/python3','-c',code)
 print('Bounded writes and scrub active')
elif mode=='fill':
 s=load();targets=s['proof_vms']
 for name in targets:
  guest(name)
  run('incus','exec',name,'--','cloud-init','status','--wait',timeout=180)
  run('incus','exec',name,'--','rm','-f','/tmp/homecloud-memory-proof-result.json')
 code="import json,time;from pathlib import Path\nM=1024**2\nmem={l.split(':')[0]:int(l.split()[1])*1024 for l in Path('/proc/meminfo').read_text().splitlines() if l.split(':')[0] in ['MemTotal','MemAvailable']}\nreserve=512*M if mem['MemTotal']>1536*M else 384*M\ntarget=max(0,mem['MemTotal']-reserve); chunks=[]\nfor i in range(target//(16*M)):\n b=bytearray(16*M)\n for j in range(0,len(b),4096):b[j]=(j//4096+i)%251\n chunks.append(b)\nPath('/tmp/homecloud-memory-proof-result.json').write_text(json.dumps({'allocated_bytes':len(chunks)*16*M,'reserve_bytes':reserve}))\ntime.sleep(180)"
 for name in targets:
  run('incus','exec',name,'--','systemd-run','--unit=homecloud-memory-pressure-proof','--collect','--property=RuntimeMaxSec=200','--property=OOMScoreAdjust=1000','/usr/bin/python3','-c',code)
 if 'scrub in progress' not in run('zpool','status','hclab').stdout:
  run('zpool','scrub','hclab');s['scrub_started']=True;save(s)
 print('Guest memory growth started in '+str(len(targets))+' VMs')
elif mode=='guest-status':
 s=load();result={}
 for name in s['proof_vms']:
  guest(name)
  code="import json,subprocess;from pathlib import Path;p=Path('/tmp/homecloud-memory-proof-result.json');unit=subprocess.run(['systemctl','is-active','homecloud-memory-pressure-proof.service'],capture_output=True,text=True).stdout.strip();oom=int(next(l.split()[1] for l in Path('/proc/vmstat').read_text().splitlines() if l.startswith('oom_kill ')));print(json.dumps({'unit':unit,'allocation':json.loads(p.read_text()) if p.exists() else None,'oom_kill':oom}))"
  result[name]=json.loads(run('incus','exec',name,'--','python3','-c',code).stdout)
 print(json.dumps(result))
elif mode=='stop-load':
 s=load()
 for name in s['proof_vms']:
  guest(name);run('incus','exec',name,'--','systemctl','stop','homecloud-memory-pressure-proof.service',check=False)
  run('incus','exec',name,'--','rm','-f','/tmp/homecloud-memory-proof-result.json',check=False)
 run('systemctl','stop','homecloud-memory-io-proof.service',check=False)
 if s['scrub_started'] and 'scrub in progress' in run('zpool','status','hclab').stdout:run('zpool','scrub','-s','hclab')
 print('Temporary guest allocation and write load stopped')
elif mode=='cleanup':
 s=load();dataset=s['dataset'];assert re.fullmatch(r'hclab/homecloud-memory-proof-[a-f0-9]{32}',dataset)
 assert run('zfs','get','-H','-o','value','homecloud:proof',dataset).stdout.strip()=='memory-policy'
 run('zfs','destroy',dataset);STATE.unlink();print('Tagged pressure dataset removed')
else:raise ValueError('Unknown proof action')
