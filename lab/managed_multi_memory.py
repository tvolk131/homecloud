#!/usr/bin/python3
"""Guarded two-user cold-boot recovery and concurrent allocation proof."""
import argparse
import json
from pathlib import Path
import time
import uuid
from control_runner import GIB
from managed_host_runner import ManagedRunner


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for flag in ('state','ssh-config','cli'):p.add_argument('--'+flag,required=True)
    p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'));args=p.parse_args()
    r=ManagedRunner(args);assert r.state['managed_host'] and not r.state.get('cleaned')
    proof=r.state.setdefault('multi',{});worker=r.state['fixtures'][0]['id']
    source=Path(__file__).with_name('managed_host_guest.py').read_text();multi=Path(__file__).with_name('managed_multi_guest.py').read_text()
    def guest(action,*a):return r.guest(multi,action,*a,timeout=85)
    def call(label,*a):return guest('call',label,*a)
    def status():return call('admin','status')
    def note(k,v):proof[k]=v;r.save();print(k,flush=True)
    def wait(predicate,label,timeout=360):
        until=time.monotonic()+timeout;last=None
        while time.monotonic()<until:
            last=status()
            if predicate(last):return last
            reserve=last['host_reserves'].get('operation')
            if reserve and reserve['phase']=='needs_review':raise RuntimeError(label+': reserve operation needs review: '+str(reserve['error']))
            bad=[(o['vm_id'],o['error']) for o in last['operations'] if o['phase']=='needs_review']
            if bad:raise RuntimeError(label+': '+str(bad))
            time.sleep(2)
        raise TimeoutError(label+': '+str(last.get('memory_health')))
    def ready(s,ids,desired):
        return all(any(v['id']==vid and v['observed']==desired and not v['error'] for v in s['vms']) for vid in ids) and not any(o['vm_id'] in ids and o['phase'] not in ('succeeded','failed') for o in s['operations'])
    def resize(memory):
        r.cli('stop',worker);r.wait([worker],'stopped');r.cli('edit',worker,'--memory',memory);r.wait([worker],'stopped');r.cli('start',worker);r.wait([worker],'running',timeout=600)
    if proof.get('passed'):print(json.dumps({'passed':True,'already_complete':True}));return
    if 'users' not in proof:
        r.guest(source,'init');note('users',guest('users'))
        r.guest("from pathlib import Path;import json,sys\nPath('/var/lib/homecloud-host/test-key.pub').write_text(sys.argv[1]);print(json.dumps({'written':True}))",Path(args.ssh_key).read_text().strip())
    users=proof['users'];ids=proof.setdefault('ids',{})
    for label in ('alice','bob'):
        if label not in ids:
            key=proof.setdefault('create_keys',{}).setdefault(label,str(uuid.uuid4()));r.save()
            op=call(label,'create',r.state['prefix']+'-'+label,'--memory',2,'--cores',1,'--disk',10,'--ssh-key','/var/lib/homecloud-host/test-key.pub','--request-id',key)
            ids[label]=op['vm_id'];r.save()
    packed=json.dumps(ids);payload=r.state['run_id']
    if 'initial' not in proof:
        s=wait(lambda s:ready(s,list(ids.values()),'running'),'initial two-user VMs')
        guest('proof',packed,payload,'write');note('initial',{'health':s['memory_health'],'processes':guest('processes',packed),'data_written':True})
    if 'promises-only' not in proof:
        resize(10)
        s=wait(lambda s:ready(s,list(ids.values()),'running') and s['memory_health']['condition']=='promises_exceed_capacity','promises-only cold boot')
        assert s['memory_health']['details']['held_bytes']==4*GIB
        note('promises-only',{'health':s['memory_health'],'processes':guest('processes',packed),'data':guest('proof',packed,payload)})
    if 'all-blocked' not in proof:
        resize(8)
        s=wait(lambda s:all(v['observed']=='stopped' for v in s['vms']) and s['memory_health']['condition']=='allocations_exceed_capacity','all startup blocked')
        assert sum(v['resources']['memory_bytes'] for v in s['vms'])==4*GIB and all(v['desired']=='running' for v in s['vms'])
        processes=guest('processes',packed);assert not any(processes.values())
        member=call('alice','status');assert member['memory_health']['condition']=='allocations_exceed_capacity' and member['memory_health'].get('details') is None
        assert [v['id'] for v in member['vms']]==[ids['alice']] and len(member['users'])==1
        note('all-blocked',{'health':s['memory_health'],'processes':processes,'owner_scoped_status':True,'claims':{v['id']:v['resources'] for v in s['vms']}})
    if 'arc-repair' not in proof:
        wait(lambda s:s['host_reserves'].get('operation') is None or s['host_reserves']['operation']['phase'] in ('succeeded','cancelled'),'previous reserve operation confirmation')
        r.guest("import json;from pathlib import Path\nPath('/sys/module/zfs/parameters/zfs_arc_max').write_text(str(3*1024**3));print(json.dumps({'injected':True}))")
        drift=status();assert drift['memory_health']['condition']=='arc_not_enforced'
        view=drift['host_reserves'];minimum=view['minimum_reserves']
        call('admin','reserve-set','--memory',minimum['memory_bytes']//GIB,'--cores',minimum['cpu_cores'],'--disk',minimum['disk_bytes']//GIB,'--arc-limit',2)
        repaired=wait(lambda s:s['memory_health']['condition']=='allocations_exceed_capacity' and s['host_reserves']['operation']['phase']=='succeeded','explicit cache policy reapplication')
        assert not any(guest('processes',packed).values())
        note('arc-repair',{'condition_during_drift':drift['memory_health']['condition'],'after':repaired['memory_health'],'explicit_backed_two_gib_policy':True})
    if 'selected-recovery' not in proof:
        call('alice','stop',ids['alice'])
        s=wait(lambda s:ready(s,[ids['bob']],'running') and any(v['id']==ids['alice'] and v['observed']=='stopped' and v['resources']['memory_bytes']==0 for v in s['vms']),'selected release and other owner recovery')
        assert s['memory_health']['condition']=='promises_exceed_capacity'
        denied=guest('try','alice','start',ids['alice']);assert not denied['accepted']
        processes=guest('processes',packed);assert len(processes['bob'])==1 and processes['alice']==[]
        note('selected-recovery',{'health':s['memory_health'],'processes':processes,'new_claim_denied':denied})
    if 'race' not in proof:
        call('bob','stop',ids['bob']);wait(lambda s:ready(s,list(ids.values()),'stopped') and all(v['resources']['memory_bytes']==0 for v in s['vms']),'stop before allocation race')
        for label in ('alice','bob'):
            vm=next(v for v in status()['vms'] if v['id']==ids[label])
            if vm['spec']['memory_gib']!=1:call(label,'edit',ids[label],'--memory',1)
        wait(lambda s:ready(s,list(ids.values()),'stopped'),'stopped resize')
        for label,uid in users.items():call('admin','grant',uid,'--memory',1,'--cores',1,'--disk',12)
        results=guest('race',packed,json.dumps(users));s=wait(lambda s:ready(s,list(ids.values()),'running'),'concurrent starts and grants')
        grants=[x for x in results if x['request'][1]=='grant'];assert sum(x['response']['accepted'] for x in grants)==1
        for label in ('alice','bob'):assert any(x['request'][0]==label and x['response']['accepted'] for x in results if x['request'][1]=='start')
        assert sum(u['grant']['memory_bytes'] for u in s['users'])==3*GIB and sum(u['committed']['memory_bytes'] for u in s['users'])==2*GIB
        cpus=[cpu for v in s['vms'] for cpu in v['cpu_ids']];assert len(cpus)==len(set(cpus))
        processes=guest('processes',packed);assert all(len(pids)==1 for pids in processes.values())
        note('race',{'health':s['memory_health'],'requests':results,'processes':processes,'data':guest('proof',packed,payload),'unique_cpu_ids':cpus})
    proof['passed']=True;r.save();print(json.dumps({'passed':True,'worker':worker,'inner':ids}),flush=True)
if __name__=='__main__':main()
