#!/usr/bin/python3
"""Cold-boot RAM growth/shrink campaign on a manifest-owned full managed host."""
import argparse
import json
from pathlib import Path
import time
import uuid
from managed_host_runner import ManagedRunner
from control_runner import GIB


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True)
    p.add_argument('--cli',required=True);p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'));args=p.parse_args()
    r=ManagedRunner(args);assert r.state['managed_host'] and not r.state.get('cleaned')
    source=Path(__file__).with_name('managed_host_guest.py').read_text();worker=r.state['fixtures'][0]['id']
    def guest(action,*argv):return r.guest(source,action,*argv,timeout=80)
    def call(*argv):return guest('call',*argv)
    def status():return call('status')
    def note(stage,data=None):
        r.state.setdefault('campaign',{})[stage]=data if data is not None else True;r.save();print(stage,flush=True)
    def wait(predicate,label,timeout=480):
        deadline=time.monotonic()+timeout;last=None
        while time.monotonic()<deadline:
            try:
                last=status()
                if predicate(last):return last
                reserve=last.get('host_reserves',{}).get('operation')
                if reserve and reserve['phase']=='needs_review':raise RuntimeError(label+': host reserves need review: '+str(reserve['error']))
                bad=[o for o in last['operations'] if o['phase']=='needs_review']
                if bad:raise RuntimeError(label+': '+str([(o['error'],o['vm_id']) for o in bad]))
            except (subprocess.TimeoutExpired,ConnectionError):pass
            time.sleep(2)
        raise TimeoutError(label+': '+str(last))
    def vm_ready(s,id):return any(v['id']==id and v['observed']=='running' and not v['error'] for v in s['vms']) and not any(o['vm_id']==id and o['phase'] not in ('succeeded','failed') for o in s['operations'])
    def outer_resize(memory):
        outer=next(v for v in r.snapshot()['vms'] if v['id']==worker)
        if outer['spec']['memory_gib']==memory and outer['desired']=='running':
            r.wait([worker],'running');return
        if outer['desired']!='stopped':r.cli('stop',worker)
        r.wait([worker],'stopped')
        if memory>8:r.grant(memory+4,3)
        outer=next(v for v in r.snapshot()['vms'] if v['id']==worker)
        if outer['spec']['memory_gib']!=memory:r.cli('edit',worker,'--memory',memory)
        r.wait([worker],'stopped')
        r.cli('start',worker);r.wait([worker],'running')
        note('outer-'+str(memory)+'-'+str(len(r.state.get('campaign',{}))),guest('probe'))
    import subprocess
    stages=r.state.setdefault('campaign',{})
    if r.state.get('campaign_passed'):
        print(json.dumps({'passed':True,'already_complete':True}));return
    if 'initialized' not in stages:
        installation=r.install_status();assert installation['application']['observation']['service']=='running',installation
        note('initialized',guest('init'))
    owner=stages['initialized']['owner']
    if 'baseline' not in stages:note('baseline',guest('probe'))
    r.state.setdefault('initial_reviewed_bytes',(stages['baseline']['memory']-4*GIB)//GIB*GIB);r.save()
    name=None;proof='managed-memory-'+r.state['run_id']
    if 'nested-vm-created' not in stages:
        call('grant',owner,'--memory',3,'--cores',1,'--disk',24)
        if 'inner_vm' not in r.state:
            r.guest("from pathlib import Path;import json,sys\nPath('/var/lib/homecloud-host/test-key.pub').write_text(sys.argv[1]);print(json.dumps({'written':True}))",Path(args.ssh_key).read_text().strip())
            key=r.state.setdefault('inner_create_key',str(uuid.uuid4()));r.save()
            op=call('create',r.state['prefix']+'-inner','--memory',2,'--cores',1,'--disk',10,'--ssh-key','/var/lib/homecloud-host/test-key.pub','--request-id',key)
            r.state['inner_vm']=op['vm_id'];r.save()
        inner=r.state['inner_vm'];wait(lambda s:vm_ready(s,inner),'initial nested VM')
        name='hc-'+uuid.UUID(inner).hex
        write="import json,subprocess,sys\nsubprocess.run(['incus','exec',sys.argv[1],'--','sh','-c','printf %s '+sys.argv[2]+' > /root/homecloud-memory-proof'],check=True);print(json.dumps({'written':True}))"
        r.guest(write,name,proof);note('nested-vm-created',guest('probe'))
    inner=r.state['inner_vm'];name='hc-'+uuid.UUID(inner).hex
    if 'growth-unissued' not in stages:
        outer_resize(36)
        grown=wait(lambda s:vm_ready(s,inner),'growth boot recovery');m=grown['host_reserves']['memory']
        assert m['safe_bytes']>m['reviewed_bytes'] and m['reviewed_bytes']==r.state['initial_reviewed_bytes']
        note('growth-unissued',{'memory':m,'grant':grown['me']['grant']})
    if 'growth-admission-blocked' not in stages:
        rejected=guest('try','grant',owner,'--memory',4,'--cores',1,'--disk',24);assert not rejected['accepted'];note('growth-admission-blocked',rejected)
    if 'coordinated-arc' not in stages:
        before=next((v for k,v in stages.items() if k.startswith('outer-36-')),None) or guest('probe');view=status()['host_reserves']
        if not view.get('operation') or view['operation']['phase']=='succeeded':
            call('reserve-set','--memory',view['minimum_reserves']['memory_bytes']//GIB,'--cores',view['minimum_reserves']['cpu_cores'],'--disk',view['minimum_reserves']['disk_bytes']//GIB,'--arc-limit',1)
        wait(lambda s:s['host_reserves']['operation'] is not None and s['host_reserves']['operation']['phase']=='succeeded','coordinated ARC cap')
        after=guest('probe');assert after['arc']['c_max']==GIB and after['arc']['c_min']<GIB
        note('coordinated-arc',{'before':before['arc'],'after':after['arc'],'parameters':after['arc_parameters']})
    if 'growth-adopted' not in stages:
        current=status();m=current['host_reserves']['memory']
        if m['reviewed_bytes']!=m['safe_bytes']:call('memory-adopt','--memory',m['safe_bytes']//GIB,'--expected-revision',m['revision'])
        adopted=status();assert adopted['host_reserves']['memory']['reviewed_bytes']==m['safe_bytes'];note('growth-adopted',adopted['host_reserves']['memory'])
    if 'held-before-shrink' not in stages:
        call('grant',owner,'--memory',8,'--cores',1,'--disk',24)
        vm=next(v for v in status()['vms'] if v['id']==inner)
        if vm['spec']['memory_gib']!=6:call('edit',inner,'--memory',6,'--restart')
        wait(lambda s:vm_ready(s,inner),'larger nested VM')
        note('held-before-shrink',{'memory':status()['me']['committed']['memory_bytes'],'probe':guest('probe')})
    if 'shrink-kept-claims-blocked-autostart' not in stages:
        outer_resize(8)
        shrink=wait(lambda s:s['host_reserves']['memory']['safe_bytes']<6*GIB and any(v['id']==inner and v['observed']=='stopped' for v in s['vms']),'held memory deficit')
        vm=next(v for v in shrink['vms'] if v['id']==inner)
        assert vm['resources']['memory_bytes']==6*GIB and vm['desired']=='running'
        probe=guest('probe');assert next(v for v in probe['vms'] if v['id']==inner)['status']=='Stopped'
        note('shrink-kept-claims-blocked-autostart',{'memory':shrink['host_reserves']['memory'],'grant':shrink['me']['grant'],'held':vm['resources'],'probe':probe})
    if 'unsafe-edit-blocked-before-intent' not in stages:
        denied=guest('try','edit',inner,'--memory',2,'--restart');assert not denied['accepted'] and 'Stop' in denied['message'];note('unsafe-edit-blocked-before-intent',denied)
    if 'released-after-shrink' not in stages:
        vm=next(v for v in status()['vms'] if v['id']==inner)
        if vm['desired']!='stopped':call('stop',inner)
        wait(lambda s:any(v['id']==inner and v['observed']=='stopped' and v['resources']['memory_bytes']==0 for v in s['vms']),'release held compute')
        note('released-after-shrink',True)
    if 'stopped-resize-after-shrink' not in stages:
        vm=next(v for v in status()['vms'] if v['id']==inner)
        if vm['spec']['memory_gib']!=2:call('edit',inner,'--memory',2)
        wait(lambda s:not any(o['vm_id']==inner and o['phase'] not in ('succeeded','failed') for o in s['operations']),'stopped resize')
        note('stopped-resize-after-shrink',True)
    if 'promise-deficit-blocks-new-claim' not in stages:
        denied=guest('try','start',inner);assert not denied['accepted'];note('promise-deficit-blocks-new-claim',denied)
    safe=status()['host_reserves']['memory']['safe_bytes']//GIB
    call('grant',owner,'--memory',safe,'--cores',1,'--disk',24);call('start',inner)
    wait(lambda s:vm_ready(s,inner),'recovery after shrink')
    read="import json,subprocess,sys\np=subprocess.run(['incus','exec',sys.argv[1],'--','cat','/root/homecloud-memory-proof'],capture_output=True,text=True,check=True);assert p.stdout==sys.argv[2];print(json.dumps({'proof_preserved':True}))"
    note('recovered-data',r.guest(read,name,proof));note('shrink-recovered',guest('probe'))
    r.state['campaign_passed']=True;r.save();print(json.dumps({'passed':True,'worker':worker,'inner':inner}),flush=True)

if __name__=='__main__':main()
