#!/usr/bin/python3
"""Disposable managed-host upgrade proofs. Never run on a working host."""
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import time

ROOT=Path('/var/lib/homecloud-host')
RECORD=ROOT/'deployment-proof.json'
SOURCE=Path('/opt/homecloud-host/source')
BUNDLE=Path('/opt/homecloud-stabilization.tar.gz')


def run(*args,input=None,timeout=90,check=True):
    p=subprocess.run(args,input=input,text=True,capture_output=True,timeout=timeout)
    if check and p.returncode:raise RuntimeError((p.stderr or p.stdout)[-2000:])
    return p


def engine():
    spec=importlib.util.spec_from_file_location('proof_deploy',SOURCE/'scripts/host_deploy.py')
    mod=importlib.util.module_from_spec(spec);spec.loader.exec_module(mod);return mod


def cli(*args):
    p=run('homecloud','--profile',str(ROOT/'lab-client.json'),*map(str,args))
    r=json.loads(p.stdout);assert r['kind']!='error',str(r.get('data'));return r.get('data')


def state():return json.loads(RECORD.read_text()) if RECORD.exists() else {}

def proof(s):
    name='hc-'+s['inner'].replace('-','')
    st=json.loads(run('incus','query','/1.0/instances/'+name+'/state').stdout)
    data=run('incus','exec',name,'--','cat','/root/deployment-proof',timeout=30).stdout
    endpoint=engine().digest(Path('/var/lib/homecloud/control/endpoint.secret').read_bytes())
    return {'pid':st['pid'],'status':st['status'],'data':data,'endpoint_sha256':endpoint,
            'incus_pid':run('systemctl','show','incus','--property=MainPID','--value').stdout.strip()}


def storage_summary(snapshot, earliest, now, smartctl_available):
    assert snapshot['me']['role']=='admin','Storage checkpoint requires an enrolled administrator'
    storage=snapshot['inventory'].get('storage')
    assert storage is not None,'Storage inventory is absent'
    for source in ('disk_collection','pool_collection'):
        observation=storage[source]
        assert not observation.get('error'),source+': '+str(observation.get('error'))
        observed=observation.get('observed_at')
        assert observed is not None and earliest<=observed<=now and now-observed<=180,source+' is not fresh'
    disks=storage['disks'];identities={disk['id'] for disk in disks}
    assert len(disks)>=2,'Boot and pool disks must both be visible'
    boot=[disk['path'] for disk in disks if any('/' in device['mountpoints'] for device in disk['devices'])]
    assert boot,'Boot mount is not associated with a disk'
    pool=next((pool for pool in storage['pools'] if pool['name']=='hcnested'),None)
    assert pool is not None and pool['state']=='ONLINE' and pool['managed'],'Managed nested pool is not ONLINE'
    def leaves(vdev):
        if not vdev['children']:return [vdev]
        return [leaf for child in vdev['children'] for leaf in leaves(child)]
    devices=[leaf for vdev in pool['vdevs'] for leaf in leaves(vdev)]
    assert devices and all(leaf.get('path') and leaf.get('disk_id') in identities for leaf in devices),'Pool leaves are not linked to disks'
    for disk in disks:
        smart=disk['smart']
        assert smart['state']!='pending','SMART observation is still pending'
        if not smartctl_available:
            assert smart['state']=='unavailable' and smart.get('passed') is None and smart['collection'].get('error'),'Missing smartctl must not imply healthy disks'
    return {'disk_count':len(disks),'boot_disks':boot,'managed_pool':pool['name'],
            'pool_state':pool['state'],'linked_vdev_leaves':len(devices),
            'disk_observed_at':storage['disk_collection']['observed_at'],
            'pool_observed_at':storage['pool_collection']['observed_at'],
            'smartctl_available':smartctl_available,
            'smart_states':{disk['path']:disk['smart']['state'] for disk in disks}}


def storage_checkpoint(earliest):
    deadline=time.monotonic()+100;last='No snapshot received'
    while time.monotonic()<deadline:
        snapshot=cli('status')
        try:summary=storage_summary(snapshot,earliest,int(time.time()),shutil.which('smartctl') is not None)
        except AssertionError as error:last=str(error);time.sleep(2);continue
        engine().save(ROOT/'storage-snapshot.json',snapshot)
        return summary
    raise RuntimeError('Candidate-active storage checkpoint failed: '+last)


def main():
    action=sys.argv[1];s=state();mod=engine() if (SOURCE/'scripts/host_deploy.py').exists() else None
    if action=='adopt':
        # Source and baseline came from the original verified worker bundle.
        # Only the new, reviewed deployment engine is added before adoption.
        with tarfile.open(BUNDLE) as archive:data=archive.extractfile('scripts/host_deploy.py').read()
        (SOURCE/'scripts/host_deploy.py').write_bytes(data);mod=engine()
        mod.read_bundle(BUNDLE,sys.argv[2]);sha=mod.digest(Path('/usr/local/bin/homecloudd').read_bytes())
        if not Path('/var/lib/homecloud/deployments/installed.json').exists():
            result=json.loads(run('python3',str(SOURCE/'scripts/host_deploy.py'),'adopt-legacy','--source',str(SOURCE),'--expected-daemon-sha256',sha).stdout)
        else:result={'status':'already adopted'}
        print(json.dumps(result));return
    assert mod is not None
    def save():mod.save(RECORD,s)
    if action=='record':
        s['inner']=sys.argv[2];s['owner']=sys.argv[3]
        name='hc-'+s['inner'].replace('-','')
        run('incus','exec',name,'--','sh','-c','printf deployment-survives > /root/deployment-proof; sync')
        s['baseline']=proof(s);save();print(json.dumps(s['baseline']));return
    if action=='check':
        before=proof(s);assert before==s['baseline']
        result=json.loads(run('python3',mod.GUARD,'check','--bundle',str(BUNDLE),'--sha256',sys.argv[2]).stdout)
        assert proof(s)==before;print(json.dumps(result));return
    if action=='install-check':
        manifest,files=mod.read_bundle(BUNDLE,sys.argv[2]);target=Path('/opt/homecloud-install-check')
        for name,data in files.items():mod.atomic(target/name,data,0o755 if name.startswith('bin/') else 0o644)
        before=proof(s)
        run('python3',str(target/'scripts/install_host.py'),'--check','--policy','/etc/homecloud/host.json','--daemon',str(target/'bin/homecloudd'),timeout=90)
        assert proof(s)==before;print(json.dumps({'install_preflight':True,'proof':proof(s)}));return
    if action=='upgrade':
        cli('grant',s['owner'],'--memory',1,'--cores',1,'--disk',24)
        started_at=int(time.time())
        result=json.loads(run('python3',mod.GUARD,'upgrade','--bundle',str(BUNDLE),'--sha256',sys.argv[2],timeout=120).stdout)
        assert proof(s)==s['baseline'];s['successful_upgrade']=result;save()
        if '--storage-check' in sys.argv[3:]:
            s['storage_checkpoint']=storage_checkpoint(started_at);save()
        # This is acknowledged *after* the deployment checkpoint. A software
        # rollback must retain it rather than restoring an old database copy.
        cli('grant',s['owner'],'--memory',2,'--cores',1,'--disk',24)
        # Allow multiple outer monitor passes to expose helper-path collisions.
        time.sleep(7);mod.Deployment().verify(mod.Deployment().receipt())
        run('python3',mod.GUARD,'rollback',result['deployment'],timeout=120)
        assert cli('status')['me']['grant']['memory_bytes']==2*1024**3
        assert proof(s)==s['baseline'];print(json.dumps({'upgrade':True,'rollback':True,'acknowledged_grant_preserved':True,'proof':proof(s),'storage':s.get('storage_checkpoint')}));return
    if action=='kill':
        manifest,files=mod.read_bundle(BUNDLE,sys.argv[2])
        pid=os.fork()
        if pid==0:
            atomic=mod.atomic
            def crash(path,data,mode=0o600):
                atomic(path,data,mode)
                if str(path)=='/usr/local/bin/homecloudd':os.kill(os.getpid(),signal.SIGKILL)
            mod.atomic=crash
            try:mod.Deployment().update(manifest,files)
            finally:os._exit(3)
        _,status=os.waitpid(pid,0);assert os.WIFSIGNALED(status) and os.WTERMSIG(status)==signal.SIGKILL
        transaction=Path('/var/lib/homecloud/deployments/transaction.json')
        assert json.loads(transaction.read_text())['phase']=='applying'
        run('systemctl','start','homecloudd')
        assert json.loads(transaction.read_text())['phase']=='rolled_back'
        assert proof(s)==s['baseline'];print(json.dumps({'sigkill':True,'guard_restored':True,'proof':proof(s)}));return
    if action=='bad-start':
        manifest,files=mod.read_bundle(BUNDLE,sys.argv[2])
        c=ROOT/'bad-daemon.c';exe=ROOT/'bad-daemon'
        c.write_text('#include <string.h>\nint main(int n,char **v){return n==2 && !strcmp(v[1],"--help")?0:42;}\n')
        run('gcc','-O2','-o',str(exe),str(c));files['bin/homecloudd']=exe.read_bytes()
        manifest['files']['bin/homecloudd']={'sha256':mod.digest(files['bin/homecloudd']),'bytes':len(files['bin/homecloudd'])}
        bad=ROOT/'bad-bundle.tgz'
        with tarfile.open(bad,'w:gz') as archive:
            for name,data in {**files,'manifest.json':json.dumps(manifest).encode()}.items():
                entry=tarfile.TarInfo(name);entry.size=len(data);archive.addfile(entry,io.BytesIO(data))
        p=run('python3',mod.GUARD,'upgrade','--bundle',str(bad),'--sha256',mod.digest(bad.read_bytes()),timeout=100,check=False)
        assert p.returncode!=0 and 'did not become ready' in p.stderr,p.stderr[-1500:]
        current=json.loads(Path('/var/lib/homecloud/deployments/installed.json').read_text())
        with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:assert db.execute('SELECT count(*) FROM users').fetchone()[0]>0
        run('python3',mod.GUARD,'rollback',current['id'],timeout=120)
        assert cli('status')['me']['grant']['memory_bytes']==2*1024**3
        assert proof(s)==s['baseline'];print(json.dumps({'bad_start_rejected':True,'explicit_rollback':True,'database_retained':True,'proof':proof(s)}));return
    raise ValueError('Unknown disposable deployment proof')


if __name__=='__main__':main()
