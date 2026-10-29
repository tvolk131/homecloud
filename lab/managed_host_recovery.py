#!/usr/bin/python3
"""Prove degraded management and one cold boot in a fresh manifest-owned host."""
import argparse
import json
from pathlib import Path
import time
import uuid
from managed_host_runner import ManagedRunner
from control_runner import GIB


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True);p.add_argument('--cli',required=True);p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'));args=p.parse_args()
    r=ManagedRunner(args);assert r.state['fresh']['passed'];proof=r.state.setdefault('recovery',{})
    source=Path(__file__).with_name('managed_host_guest.py').read_text();inner=r.state['fresh']['inner_id'];worker=r.state['fixtures'][0]['id'];name='hc-'+uuid.UUID(inner).hex
    def guest(action,*a):return r.guest(source,action,*a,timeout=80)
    def call(*a):return guest('call',*a)
    def root(code,*a):return r.guest(code,*a,timeout=90)
    def note(k,v):proof[k]=v;r.save();print(k,flush=True)
    def wait(predicate,label,timeout=300):
        until=time.monotonic()+timeout
        while time.monotonic()<until:
            s=call('status')
            if predicate(s):return s
            if any(o['phase']=='needs_review' for o in s['operations']):raise RuntimeError(label+': operation needs review')
            time.sleep(2)
        raise TimeoutError(label)
    def stopped(s):return any(v['id']==inner and v['observed']=='stopped' and v['resources']['memory_bytes']==0 for v in s['vms'])
    def running(s):return any(v['id']==inner and v['observed']=='running' and not v['error'] for v in s['vms']) and not any(o['vm_id']==inner and o['phase'] not in ('succeeded','failed') for o in s['operations'])
    if not proof.get('incus-unavailable'):
        call('stop',inner);wait(stopped,'stop before service fault')
        root("import json,subprocess\nsubprocess.run(['systemctl','--no-block','stop','incus.service','incus.socket'],check=True);print(json.dumps({'stopping':True}))")
        start=time.monotonic();s=wait(lambda s:any(v['id']==inner and v['observed']=='unavailable' for v in s['vms']),'unavailable observation')
        assert s['host_reserves'] is not None and s['me']['committed']['memory_bytes']==0 and s['me']['committed']['disk_bytes']==12*GIB
        assert any('Incus inventory unavailable' in w for w in s['inventory']['warnings'])
        note('incus-unavailable',{'seconds':round(time.monotonic()-start,2),'warnings':s['inventory']['warnings'],'held':s['me']['committed']})
    root("import json,subprocess\nsubprocess.run(['systemctl','--no-block','start','incus.socket','incus.service','incus-startup.service'],check=True);print(json.dumps({'starting':True}))")
    current=next(v for v in call('status')['vms'] if v['id']==inner)
    if current['desired']!='stopped':call('stop',inner)
    wait(stopped,'Incus recovery')
    if not proof.get('arc-drift'):
        root("import json;from pathlib import Path\nPath('/sys/module/zfs/parameters/zfs_arc_max').write_text(str(3*1024**3));print(json.dumps({'injected':True}))")
        s=call('status');assert not s['host_reserves']['memory']['arc_ready']
        denied=guest('try','start',inner);assert not denied['accepted'] and call('status')['me']['committed']['memory_bytes']==0
        deadline=time.monotonic()+45
        while time.monotonic()<deadline:
            app=r.install_status()['application'];components=app['observation']['components']
            if any(c['name']=='Memory policy' and not c['ready'] for c in components):break
            time.sleep(2)
        else:raise AssertionError('Outer managed-host readiness did not show memory-policy drift')
        observed=guest('probe');assert observed['arc']['c_max']==3*GIB
        note('arc-drift',{'memory':s['host_reserves']['memory'],'denied':denied,'outer_components':components,'inspection_did_not_repair':True})
    root("import json,sys\nsys.path.insert(0,'/usr/local/libexec/homecloud');import host_runtime as h\nc=h.load_config();h.resource_reserves.arc_policy.apply(h,c,c['_reserve_record']['state']['arc'],persist=False);print(json.dumps({'restored':True}))")
    wait(lambda s:s['host_reserves']['memory']['arc_ready'],'ARC recovery')
    call('start',inner);wait(running,'start after service recovery')
    if not proof.get('cold-boot'):
        data='fresh-recovery-'+r.state['run_id']
        writer="import os,sys;from pathlib import Path\nf=open('/root/homecloud-fresh-proof','w');f.write(sys.argv[1]);f.flush();os.fsync(f.fileno());f.close()\nfd=os.open('/root',os.O_RDONLY|os.O_DIRECTORY);os.fsync(fd);os.close(fd)\nPath('/root/homecloud-unsynced-proof').write_text(sys.argv[1])"
        root("import json,subprocess,sys\nsubprocess.run(['systemctl','is-active','--quiet','incus-startup.service'],check=True);subprocess.run(['incus','exec',sys.argv[1],'--','python3','-c',sys.argv[3],sys.argv[2]],check=True);print(json.dumps({'written':True,'shutdown_hook_active':True}))",name,data,writer)
        before=guest('probe');r.cli('stop',worker);r.wait([worker],'stopped');r.cli('start',worker);r.wait([worker],'running',timeout=600)
        wait(running,'cold boot inner recovery')
        result=root("import json,subprocess,sys\np=subprocess.run(['incus','exec',sys.argv[1],'--','cat','/root/homecloud-fresh-proof','/root/homecloud-unsynced-proof'],capture_output=True,text=True,check=True);assert p.stdout==sys.argv[2]*2;print(json.dumps({'fsynced_and_buffered_proofs_preserved':True}))",name,data)
        after=guest('probe');assert after['boot_id']!=before['boot_id'] and after['arc']['c_max']==GIB
        note('cold-boot',{'before':before,'after':after,'data':result})
    proof['passed']=True;r.save();print(json.dumps({'passed':True}),flush=True)

if __name__=='__main__':main()
