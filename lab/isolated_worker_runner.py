"""Disposable nested worker and real persistence faults, never the working devbox."""
from pathlib import Path
import argparse,json,sys,textwrap,time
from control_runner import Runner,atomic_json
ROOT=Path(__file__).resolve().parents[1]
def main():
 p=argparse.ArgumentParser(description=__doc__)
 p.add_argument('action',choices=['prepare','persistence','memory','cleanup'])
 p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True)
 p.add_argument('--cli',default=str(ROOT/'target/debug/homecloud'))
 p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'))
 p.add_argument('--evidence-dir',required=True);p.add_argument('--resume',action='store_true')
 a=p.parse_args();r=Runner(a);out=Path(a.evidence_dir);out.mkdir(parents=True,exist_ok=True,mode=0o700)
 if a.action=='cleanup':r.cleanup();return
 if a.action=='prepare':
  if r.state:
   assert a.resume,'Existing proof: inspect it, then pass --resume to prepare again'
   r.discover();assert len(r.state['fixtures'])==1 and r.state['fixtures'][0]['memory']==7
   r.state['worker']=r.state['fixtures'][0]['id'];r.save();assert r.state['worker']
  else:
   r.start_run();ids=r.create_batch(1,memory=7,disk=32);r.state['worker']=ids[0];r.save()
  r.wait([r.state['worker']],'running')
 vid=r.state['worker'];name='hc-'+vid.replace('-','')
 files={n:(ROOT/'crates/host'/n).read_text() for n in ['arc_policy.py','resource_reserves.py']}
 for n in ['persistence_guest.py','persistence_faults.py','nested_memory.py']:files[n]=(ROOT/'lab'/n).read_text()
 prelude='''from pathlib import Path
import json,subprocess,sys,time,sqlite3,uuid
vid=sys.argv[1];name='hc-'+vid.replace('-','')
v=json.loads(subprocess.check_output(['incus','query','/1.0/instances/'+name]));assert v['config']['user.homecloud.id']==vid
assert v['config']['user.homecloud.owner']==sys.argv[2]
with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
 managed=json.loads(db.execute('SELECT record FROM vms WHERE id=?',[vid]).fetchone()[0])
 assert managed['owner']==sys.argv[2] and managed['spec']['name'].startswith('hc-proof-') and not managed['spec'].get('applications')
 original_claim=managed['resources']
permanent='hc-e8acf92ed8294f44b844cc12d10c6344'
permanent_pid=json.loads(subprocess.check_output(['incus','query','/1.0/instances/'+permanent+'/state']))['pid']
def verify_protected():
 with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
  assert json.loads(db.execute('SELECT record FROM vms WHERE id=?',[vid]).fetchone()[0])['resources']==original_claim
 assert json.loads(subprocess.check_output(['incus','query','/1.0/instances/'+permanent+'/state']))['pid']==permanent_pid
def execute(*args,timeout=240):return subprocess.run(['incus','exec',name,'--',*args],capture_output=True,text=True,timeout=timeout)
def checked(*args,timeout=240):
 p=execute(*args,timeout=timeout);assert p.returncode==0,p.stderr+p.stdout;return p.stdout
'''
 if a.action=='prepare':
  code=prelude+'''
checked('touch','/etc/homecloud-recovery-proof')
checked('sh','-c','set -eu; apt-get update -qq; DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zfsutils-linux initramfs-tools linux-modules-extra-$(uname -r) incus qemu-system-x86 > /var/log/homecloud-recovery-install.log 2>&1; modprobe zfs; modprobe kvm_intel',timeout=650)
FILES=FILES_LITERAL
writer="from pathlib import Path;import json,sys;root=Path('/opt/homecloud-recovery-proof');root.mkdir(exist_ok=True);[(root/n).write_text(s) for n,s in json.load(sys.stdin).items()]"
p=subprocess.run(['incus','exec',name,'--','python3','-c',writer],input=json.dumps(FILES),text=True,capture_output=True,timeout=20);assert not p.returncode,p.stderr
unit='[Unit]\\nDescription=Disposable Homecloud ARC recovery proof\\nAfter=systemd-modules-load.service\\nBefore=multi-user.target\\nConditionPathExists=/etc/homecloud-recovery-proof\\n[Service]\\nType=oneshot\\nExecStart=/usr/bin/python3 /opt/homecloud-recovery-proof/persistence_guest.py recover\\nRemainAfterExit=yes\\n[Install]\\nWantedBy=multi-user.target\\n'
setup="from pathlib import Path;Path('/etc/modules-load.d/homecloud-recovery.conf').write_text('zfs\\\\n');Path('/etc/systemd/system/homecloud-recovery-proof.service').write_text("+repr(unit)+")"
checked('python3','-c',setup);checked('systemd-analyze','verify','/etc/systemd/system/homecloud-recovery-proof.service');checked('systemctl','daemon-reload');checked('systemctl','enable','homecloud-recovery-proof.service')
# Export exactly the reviewed parent image; no guest or credential copying.
c=json.loads(Path('/etc/homecloud/host.json').read_text());dest=Path('/var/lib/homecloud/test-runs')/('image-'+vid);dest.mkdir(exist_ok=True,mode=0o700)
subprocess.run(['incus','image','export',c['image'],str(dest/'image')],check=True,capture_output=True,timeout=120)
image_files=list(dest.iterdir());assert 1<=len(image_files)<=2
checked('mkdir','-p','/opt/homecloud-recovery-proof/image')
for f in image_files:subprocess.run(['incus','file','push',str(f),name+'/opt/homecloud-recovery-proof/image/'+f.name],check=True,capture_output=True,timeout=120)
for f in image_files:f.unlink()
dest.rmdir()
print(json.dumps({'worker':vid,'nested_kvm':checked('test','-c','/dev/kvm')=='','installed':True}))
'''.replace('FILES_LITERAL',repr(files))
  result=r.remote(code,vid,r.state['owner'],timeout=1000);atomic_json(out/'worker-prepare.json',result);print(result,flush=True);return
 if a.action=='persistence':
  code=prelude+'''
def inspect():return json.loads(checked('python3','/opt/homecloud-recovery-proof/persistence_faults.py','inspect',timeout=20))
def reboot(hard=False):
 if hard:
  subprocess.run(['incus','stop',name,'--force'],check=True,capture_output=True,timeout=60)
  subprocess.run(['incus','start',name],check=True,capture_output=True,timeout=60)
 else:subprocess.run(['incus','restart',name,'--timeout','30'],check=True,capture_output=True,timeout=90)
 end=time.monotonic()+240;after=None
 while time.monotonic()<end:
  p=execute('python3','/opt/homecloud-recovery-proof/persistence_faults.py','inspect',timeout=15)
  if not p.returncode:
   after=json.loads(p.stdout)
   if after['journal']['phase']=='applied':return after
  time.sleep(2)
 raise RuntimeError('VM did not recover: '+str(after))
results=[]
for fault in ['config-readonly','boot-full']:
 p=execute('python3','/opt/homecloud-recovery-proof/persistence_faults.py','filesystem',fault,timeout=300)
 assert p.returncode==0,p.stderr+p.stdout
 before=json.loads(p.stdout);after=reboot()
 assert before['before']['boot']!=after['boot'] and after['arc']['maximum_bytes']==256*1024**2
 verify_protected()
 results.append({'kind':fault,'injection':before,'after':after})
 print('filesystem '+fault+' passed',file=sys.stderr,flush=True)
for point in ['before-initramfs','during-initramfs','after-initramfs']:
 nonce=str(uuid.uuid4())
 checked('systemd-run','--unit=homecloud-power-loss-proof','--collect','--property=RuntimeMaxSec=650','python3','/opt/homecloud-recovery-proof/persistence_faults.py','power',point,nonce)
 end=time.monotonic()+240;before=None
 while time.monotonic()<end:
  p=execute('cat','/var/lib/homecloud-recovery-proof/power-checkpoint.json',timeout=15)
  if p.returncode==0:
   candidate=json.loads(p.stdout)
   if candidate['point']==point and candidate.get('attempt')==nonce:before=candidate;break
  time.sleep(.2)
 assert before,'Fault checkpoint was not reached'
 after=reboot(hard=True)
 assert before['before']['boot']!=after['boot'] and after['arc']['maximum_bytes']==256*1024**2
 assert not after['journal'].get('arc_persistence_required') and 'arc_previous_sys_free' not in after['journal']
 verify_protected()
 results.append({'kind':'virtual-hard-power-off','point':point,'before':before,'after':after})
 print('power '+point+' passed',file=sys.stderr,flush=True)
print(json.dumps({'passed':True,'cases':results}))
'''
  result=r.remote(code,vid,r.state['owner'],timeout=1500)
  for case in result['cases']:
   before=case['injection']['before'] if case['kind']!='virtual-hard-power-off' else case['before']['before']
   assert case['after']['temporary_reclaim_target']==before['journal']['arc_previous_sys_free']
  atomic_json(out/'persistence-faults.json',result);print('Five real persistence faults passed',flush=True);return
 code=prelude+'''
p=execute('python3','/opt/homecloud-recovery-proof/nested_memory.py',timeout=1900)
assert not p.returncode,p.stderr+p.stdout
print(p.stdout)
'''
 result=r.remote(code,vid,r.state['owner'],timeout=2000);atomic_json(out/'nested-memory.json',result);print('Isolated nested worker memory test passed',flush=True)
if __name__=='__main__':main()
