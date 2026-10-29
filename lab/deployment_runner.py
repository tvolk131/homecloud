#!/usr/bin/python3
"""Run software deployment proofs only in a manifest-owned managed host.

A prior managed_host_runner.py boot must finish installation. This command
never targets the permanent devbox. Failures retain the worker for inspection.
"""
import argparse,json,re,time,uuid
from pathlib import Path
from managed_host_runner import ManagedRunner

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('action',choices=['copy','adopt','inner','check','install-check','upgrade','kill','bad-start'])
p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True)
p.add_argument('--cli',default=str(Path(__file__).resolve().parents[1]/'target/debug/homecloud'))
p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'))
p.add_argument('--bundle-sha256');p.add_argument('--host-bundle',default='/tmp/homecloud-stabilization.tar.gz')
a=p.parse_args();repo=Path(__file__).resolve().parents[1]
if a.action!='inner' and (not a.bundle_sha256 or not re.fullmatch('[0-9a-f]{64}',a.bundle_sha256)):
    p.error('--bundle-sha256 is required')
r=ManagedRunner(a);action=a.action;source=Path(__file__).with_name('deployment_guest.py').read_text();digest=a.bundle_sha256
def guest(*args,timeout=150):return r.guest(source,*args,timeout=timeout)
def call(*args):return r.guest((repo/'lab/managed_host_guest.py').read_text(),'call',*args,timeout=80)
def note(key,result):r.state.setdefault('deployment',{})[key]=result;r.save();print(json.dumps({key:result}),flush=True)
if action=='copy':
 e=r.state['fixtures'][0]
 code='''import hashlib,json,subprocess,sys,uuid
from pathlib import Path
vid,owner,name,bundle,expected=sys.argv[1:];target='hc-'+uuid.UUID(vid).hex
v=json.loads(subprocess.check_output(['incus','query','/1.0/instances/'+target]));assert v['config']['user.homecloud.id']==vid and v['config']['user.homecloud.owner']==owner
assert hashlib.sha256(Path(bundle).read_bytes()).hexdigest()==expected
subprocess.run(['incus','file','push',bundle,target+'/opt/homecloud-stabilization.tar.gz'],check=True,capture_output=True)
print(json.dumps({'copied':True}))'''
 note('copied',r.remote(code,e['id'],r.state['owner'],e['name'],a.host_bundle,digest))
elif action=='adopt':note('adopted',guest('adopt',digest))
elif action=='inner':
 guestsource=(repo/'lab/managed_host_guest.py').read_text();inner=r.state.setdefault('deployment',{})
 if not inner.get('owner'):inner['owner']=r.guest(guestsource,'init',timeout=80)['owner'];r.save()
 owner=inner['owner'];call('grant',owner,'--memory',1,'--cores',1,'--disk',24)
 if not inner.get('inner'):
  key=Path(a.ssh_key)
  r.guest("import json,sys;from pathlib import Path\nPath('/var/lib/homecloud-host/test-key.pub').write_text(sys.argv[1]);print(json.dumps({'written':True}))",key.read_text().strip())
  inner.setdefault('request_id',str(uuid.uuid4()));r.save()
  op=call('create',r.state['prefix']+'-deploy-inner','--memory',1,'--cores',1,'--disk',10,'--ssh-key','/var/lib/homecloud-host/test-key.pub','--request-id',inner['request_id']);inner['inner']=op['vm_id'];r.save()
 until=time.monotonic()+420
 while time.monotonic()<until:
  s=call('status');v=next(v for v in s['vms'] if v['id']==inner['inner'])
  if v['observed']=='running' and not v['error'] and not any(o['vm_id']==v['id'] and o['phase'] not in ('succeeded','failed') for o in s['operations']):break
  if any(o['vm_id']==v['id'] and o['phase']=='needs_review' for o in s['operations']):raise RuntimeError(v['error'])
  time.sleep(2)
 else:raise TimeoutError('inner VM readiness')
 note('baseline',guest('record',inner['inner'],owner))
else:note(action,guest(action,digest,timeout=200))
