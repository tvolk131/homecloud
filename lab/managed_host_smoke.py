#!/usr/bin/python3
"""Fresh installation proof, independently enrolled guest, and first VM/ARC change."""
import argparse
import json
from pathlib import Path
import time
import uuid
from managed_host_runner import ManagedRunner
from control_runner import GIB


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True);p.add_argument('--cli',required=True);p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'));args=p.parse_args()
    r=ManagedRunner(args);assert r.state['managed_host'] and not r.state.get('cleaned')
    source=Path(__file__).with_name('managed_host_guest.py').read_text();fresh=r.state.setdefault('fresh',{})
    def guest(action,*a):return r.guest(source,action,*a,timeout=80)
    def call(*a):return guest('call',*a)
    def note(key,value):fresh[key]=value;r.save();print(key,flush=True)
    def wait(predicate,label,timeout=600):
        until=time.monotonic()+timeout
        while time.monotonic()<until:
            s=call('status')
            if predicate(s):return s
            review=[o for o in s['operations'] if o['phase']=='needs_review']
            if review:raise RuntimeError(label+': '+str(review[0]['error']))
            time.sleep(2)
        raise TimeoutError(label)
    if not fresh.get('installed'):
        deadline=time.monotonic()+1200
        while time.monotonic()<deadline:
            s=r.snapshot();vid=r.state['fixtures'][0]['id']
            if any(o['vm_id']==vid and o['phase']=='needs_review' for o in s['operations']):raise RuntimeError('Outer VM needs review')
            app=next((a for a in s['applications'] if a['vm_id']==vid),None)
            op=next((o for o in reversed(s['application_operations']) if app and o['application_id']==app['id']),None)
            if op and op['phase'] in ('failed','interrupted'):raise RuntimeError(app['observation']['operation_message'])
            if app and app['observation']['service']=='running' and op and op['phase']=='succeeded':
                note('installed',{'application':app,'operation':op});break
            time.sleep(3)
        else:raise TimeoutError('Fresh installation did not finish')
    if not fresh.get('enrolled'):note('enrolled',guest('init'))
    owner=fresh['enrolled']['owner']
    if not fresh.get('baseline'):note('baseline',guest('probe'))
    if not fresh.get('inner_id'):
        call('grant',owner,'--memory',3,'--cores',1,'--disk',24)
        r.guest("import json,sys;from pathlib import Path\nPath('/var/lib/homecloud-host/test-key.pub').write_text(sys.argv[1]);print(json.dumps({'written':True}))",Path(args.ssh_key).read_text().strip())
        key=fresh.setdefault('create_key',str(uuid.uuid4()));r.save()
        op=call('create',r.state['prefix']+'-inner','--memory',2,'--cores',1,'--disk',10,'--ssh-key','/var/lib/homecloud-host/test-key.pub','--request-id',key)
        note('inner_id',op['vm_id'])
    inner=fresh['inner_id']
    wait(lambda s:any(v['id']==inner and v['observed']=='running' and not v['error'] for v in s['vms']) and not any(o['vm_id']==inner and o['phase'] not in ('succeeded','failed') for o in s['operations']),'fresh inner VM')
    note('first-vm',guest('probe'))
    if not fresh.get('first-arc-change'):
        view=call('status')['host_reserves']
        if view['operation'] is None:
            call('reserve-set','--memory',4,'--cores',1,'--disk',2,'--arc-limit',1)
        wait(lambda s:s['host_reserves']['operation'] is not None and s['host_reserves']['operation']['phase']=='succeeded','first ARC operation')
        note('first-arc-change',guest('probe'))
    proof=r.guest("import json;from pathlib import Path\nc=json.loads(Path('/etc/homecloud/host.json').read_text());print(json.dumps({'transport':c['cloud_init_transport'],'excluded_network_slots':c.get('excluded_network_slots',[])}))")
    outer=next(v for v in r.snapshot()['vms'] if v['id']==r.state['fixtures'][0]['id'])
    inner_vm=next(v for v in call('status')['vms'] if v['id']==inner)
    assert proof['transport']=='agent' and outer['network_slot'] in proof['excluded_network_slots'] and inner_vm['network_slot'] not in proof['excluded_network_slots']
    note('profile',proof)
    fresh['passed']=True;r.save();print(json.dumps({'passed':True,'worker':r.state['fixtures'][0]['id'],'inner':inner}),flush=True)

if __name__=='__main__':main()
