"""Prepare/exercise/clean up marked disposable VMs for real ARC boot and DNS faults."""
from pathlib import Path
import argparse,json,sys,time,textwrap
from control_runner import Runner
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('action',choices=['prepare','exercise','cleanup'])
p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True)
p.add_argument('--cli',default=str(ROOT/'target/debug/homecloud'))
p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'))
p.add_argument('--evidence-dir',required=True)
args=p.parse_args();r=Runner(args);evidence=Path(args.evidence_dir);evidence.mkdir(parents=True,exist_ok=True,mode=0o700)
if args.action=='cleanup':r.cleanup();sys.exit()
if args.action=='prepare':
    r.start_run();ids=r.create_batch(1,memory=4,disk=20);extra=r.create_batch(2);r.wait(ids+extra,'running');print(json.dumps({'id':ids[0],'name':'hc-'+ids[0].replace('-','')}),flush=True)
    code='''
    from pathlib import Path
    import json,subprocess,sys
    vid=sys.argv[1];name='hc-'+vid.replace('-','')
    v=json.loads(subprocess.check_output(['incus','query','/1.0/instances/'+name]));assert v['config']['user.homecloud.id']==vid
    subprocess.run(['incus','exec',name,'--','sh','-c','set -eu; touch /etc/homecloud-recovery-proof; apt-get update -qq; DEBIAN_FRONTEND=noninteractive apt-get install -y -qq zfsutils-linux initramfs-tools linux-modules-extra-$(uname -r) > /var/log/homecloud-recovery-install.log 2>&1; modprobe zfs'],check=True,timeout=600)
    print(json.dumps({'prepared':vid}))
    '''
    print(r.remote(textwrap.dedent(code),ids[0],timeout=650),flush=True)
    sys.exit()
assert r.state and r.state["fixtures"][0]["memory"]==4
vid=r.state['fixtures'][0]['id'];name='hc-'+vid.replace('-','')
files={n:(ROOT/'crates/host'/n).read_text() for n in ['arc_policy.py','resource_reserves.py']}
for n in ['persistence_guest.py','dns_guest.py']:files[n]=(ROOT/'lab'/n).read_text()
code='''from pathlib import Path
import json,subprocess,sys,time
vid=sys.argv[1];name='hc-'+vid.replace('-','')
v=json.loads(subprocess.check_output(['incus','query','/1.0/instances/'+name]));assert v['config']['user.homecloud.id']==vid
FILES=FILES_LITERAL
writer="from pathlib import Path;import json,sys;root=Path('/opt/homecloud-recovery-proof');root.mkdir(exist_ok=True);[(root/n).write_text(s) for n,s in json.load(sys.stdin).items()]"
subprocess.run(['incus','exec',name,'--','python3','-c',writer],input=json.dumps(FILES),text=True,check=True)
unit='[Unit]\\nDescription=Disposable Homecloud ARC persistence recovery proof\\nAfter=systemd-modules-load.service\\nBefore=multi-user.target\\nConditionPathExists=/etc/homecloud-recovery-proof\\n[Service]\\nType=oneshot\\nExecStart=/usr/bin/python3 /opt/homecloud-recovery-proof/persistence_guest.py recover\\nRemainAfterExit=yes\\n[Install]\\nWantedBy=multi-user.target\\n'
setup="from pathlib import Path;Path('/etc/modules-load.d/homecloud-recovery.conf').write_text('zfs\\\\n');Path('/etc/systemd/system/homecloud-recovery-proof.service').write_text("+repr(unit)+")"
subprocess.run(['incus','exec',name,'--','python3','-c',setup],check=True)
subprocess.run(['incus','exec',name,'--','systemctl','daemon-reload'],check=True)
subprocess.run(['incus','exec',name,'--','systemctl','enable','homecloud-recovery-proof.service'],check=True,capture_output=True)
def execute(*args,timeout=240):
 return subprocess.run(['incus','exec',name,'--',*args],capture_output=True,text=True,timeout=timeout)
results=[]
for point in ['before-initramfs','during-initramfs','after-initramfs']:
 p=execute('python3','/opt/homecloud-recovery-proof/persistence_guest.py','inject',point)
 assert p.returncode!=0,'Injected process should die'
 before=json.loads(execute('python3','/opt/homecloud-recovery-proof/persistence_guest.py','inspect').stdout)
 assert before['journal']['phase']=='applying' and before['journal']['arc_persistence_required']
 p=subprocess.run(['incus','restart',name,'--timeout','30'],capture_output=True,text=True,timeout=90);assert not p.returncode,p.stderr
 end=time.monotonic()+240;after=None
 while time.monotonic()<end:
  p=execute('python3','/opt/homecloud-recovery-proof/persistence_guest.py','inspect',timeout=20)
  if not p.returncode:
   after=json.loads(p.stdout)
   if after['journal']['phase']=='applied':break
  time.sleep(2)
 assert after and after['journal']['phase']=='applied',after
 assert after['boot']!=before['boot']
 assert after['arc']['maximum_bytes']==256*1024**2 and after['temporary_reclaim_target']==before['journal']['arc_previous_sys_free']
 assert not after['journal'].get('arc_persistence_required') and 'arc_previous_sys_free' not in after['journal']
 results.append({'point':point,'before':before,'after':after})
print(json.dumps({'persistence':results,'passed':True}))
'''.replace('FILES_LITERAL',repr(files))
result=r.remote(code,vid,timeout=900);(evidence/'persistence.json').write_text(json.dumps(result,indent=2)+'\n');print('All three real interrupted initramfs recovery cases passed',flush=True)
dns_code='''import json,subprocess,sys
name='hc-'+sys.argv[1].replace('-','')
p=subprocess.run(['incus','exec',name,'--','python3','/opt/homecloud-recovery-proof/dns_guest.py'],capture_output=True,text=True,timeout=160)
assert p.returncode==0,p.stderr
print(p.stdout)
'''
result=r.remote(dns_code,vid,timeout=180);(evidence/'dns-faults.json').write_text(json.dumps(result,indent=2)+'\n');print(result,flush=True)
