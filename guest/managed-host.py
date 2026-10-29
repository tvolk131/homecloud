#!/usr/bin/python3
"""Root-owned fixed managed host installer; no user commands or host secrets."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import time
import uuid

STATE=Path('/var/lib/homecloud-host')
ROOT=Path('/opt/homecloud-host/source')
POLICY=Path('/etc/homecloud/host.json')
RECEIPT=Path('/var/lib/homecloud/deployments/installed.json')
BUNDLE=Path('/opt/homecloud-host-bundle.tar.gz')
VERSION='0.1.0'
GIB=1024**3


def save(record):
    STATE.mkdir(parents=True,exist_ok=True,mode=0o700)
    p=STATE/'application.json';temp=p.with_suffix('.tmp')
    with temp.open('w') as f:json.dump(record,f);f.flush();os.fsync(f.fileno())
    temp.chmod(0o600);temp.replace(p)
    fd=os.open(STATE,os.O_RDONLY|os.O_DIRECTORY)
    try:os.fsync(fd)
    finally:os.close(fd)


def run(*args,**kwargs):
    p=subprocess.run(args,text=True,capture_output=True,timeout=kwargs.pop('timeout',120),**kwargs)
    if p.returncode:raise RuntimeError(args[0]+': '+(p.stderr or p.stdout)[-1200:])
    return p.stdout.strip()


def inspect():
    state=json.loads((STATE/'application.json').read_text()) if (STATE/'application.json').exists() else {}
    components=[]
    for name,command in [('Homecloud',['systemctl','is-active','homecloudd']),('Incus',['systemctl','is-active','incus']),('ZFS pool',['zpool','list','-H','-o','health','hcnested'])]:
        try:value=run(*command,timeout=5);ready=value in ('active','ONLINE')
        except (RuntimeError,subprocess.TimeoutExpired,FileNotFoundError):value='unavailable';ready=False
        components.append({'name':name,'version':value,'ready':ready,'account':''})
    try:
        policy=json.loads(run('python3','/usr/local/libexec/homecloud/host_runtime.py',input='{"kind":"reserve_status"}',timeout=8))['data']
        enforced=bool(policy['memory']['arc_ready']) and not policy['applying']
        value='enforced' if enforced else 'needs review'
    except (RuntimeError,subprocess.TimeoutExpired,FileNotFoundError,KeyError,TypeError,ValueError):enforced=False;value='unavailable'
    components.append({'name':'Memory policy','version':value,'ready':enforced,'account':''})
    ready=bool(state.get('installed')) and all(c['ready'] for c in components)
    return {'observation':{'installed_version':VERSION if state.get('installed') else None,
        'service':'running' if ready else 'unavailable','setup':'ready' if ready else 'required',
        'message':'Host is ready. Bootstrap its independent administrator from its terminal.' if ready else 'Host installation or recovery needs attention.',
        'observed_at':int(time.time()),'action':None,'accounts':[],'components':components,
        'operation_message':state.get('message',''),'operation_stage':state.get('stage','')},'job':state.get('job')}


def install(operation,digest,wwn):
    uuid.UUID(operation);assert len(digest)==64 and all(c in '0123456789abcdef' for c in digest)
    assert len(wwn)==18 and wwn.startswith('0x5000') and all(c in '0123456789abcdef' for c in wwn[2:])
    STATE.mkdir(parents=True,exist_ok=True,mode=0o700)
    with (STATE/'install.lock').open('a') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        state=json.loads((STATE/'application.json').read_text()) if (STATE/'application.json').exists() else {}
        state['job']={'id':operation,'phase':'running'}
        def stage(name,message):state.update(stage=name,message=message);save(state)
        try:
            policy=POLICY
            assert not policy.exists() or RECEIPT.exists(),'Legacy host needs explicit local adoption before automatic software updates'
            if not policy.exists():
                stage('packages','Installing Incus, ZFS and host prerequisites…')
                assert Path('/dev/kvm').exists(),'Nested KVM is unavailable; enable nested virtualization on the outer host'
                run('apt-get','update',timeout=240)
                run('apt-get','install','-y','incus','qemu-system-x86','zfsutils-linux','nftables','gcc','libc6-dev','psmisc','pciutils','sudo','curl','genisoimage',timeout=900,env={**os.environ,'DEBIAN_FRONTEND':'noninteractive'})
                run('modprobe','zfs')
            stage('bundle','Verifying and extracting the host software bundle…')
            bundle=BUNDLE;assert hashlib.sha256(bundle.read_bytes()).hexdigest()==digest,'Bundle digest mismatch'
            ROOT.mkdir(parents=True,exist_ok=True)
            with tarfile.open(bundle) as archive:
                for member in archive.getmembers():
                    path=Path(member.name)
                    assert not path.is_absolute() and '..' not in path.parts and (member.isfile() or member.isdir()),'Unsafe bundle member'
                    assert path.parts and path.parts[0] in ('bin','scripts','crates','guest','manifest.json'),'Unexpected bundle member'
                archive.extractall(ROOT,filter='data')
            # Bundles use stable timestamps. A later equal-size source update
            # must not reuse an older bytecode cache from this source tree.
            for cached in ROOT.rglob('__pycache__/*.pyc'):cached.unlink()
            if policy.exists():
                stage('controller','Applying a verified Homecloud software update…')
                run('python3','/usr/local/libexec/homecloud-deploy.py','upgrade','--bundle',str(bundle),'--sha256',digest,timeout=600)
                state.update(installed=True,stage='ready',message='Software updated. Existing host policy and guests were retained.')
                state['job']['phase']='succeeded';save(state);return
            stage('pool','Preparing the dedicated, backed test disk…')
            disk=Path('/dev/disk/by-id/wwn-'+wwn);assert disk.exists(),'Expected test disk WWN is missing'
            assert int(run('blockdev','--getsize64',str(disk)))==32*GIB,'Unexpected test disk size'
            identity={'wwn':wwn,'bytes':32*GIB}
            journal=STATE/'pool-intent.json'
            if journal.exists():assert json.loads(journal.read_text())==identity,'Pool disk identity changed; inspect before continuing'
            else:
                assert not run('wipefs','--no-act','--json',str(disk)) or not json.loads(run('wipefs','--no-act','--json',str(disk))).get('signatures'),'Test disk contains unrecognized data'
                with journal.open('x') as f:json.dump(identity,f);f.flush();os.fsync(f.fileno())
                journal.chmod(0o600)
            exists=subprocess.run(['zpool','list','-H','hcnested'],capture_output=True).returncode==0
            if not exists:
                labels=subprocess.run(['zdb','-l',str(disk)],capture_output=True,text=True)
                if labels.returncode==0:
                    # Retry after a crash between pool creation and acknowledgement.
                    run('zpool','import','-d','/dev/disk/by-id','hcnested')
                else:run('zpool','create','-o','ashift=12','-O','mountpoint=none','hcnested',str(disk))
            status=run('zpool','status','-P','hcnested');assert str(disk.resolve()) in status or str(disk) in status,'Pool uses an unexpected vdev'
            stage('image','Caching the inner guest image…')
            run('systemctl','enable','--now','incus')
            run('incus','image','copy','images:ubuntu/26.04/cloud','local:','--vm','--alias','homecloud-nested-guest',timeout=600)
            image=json.loads(run('incus','image','list','homecloud-nested-guest','--format=json'))[0]['fingerprint']
            groups=sorted({tuple(int(v) for part in p.read_text().strip().split(',') for v in range(int(part.split('-')[0]),int(part.split('-')[-1])+1)) for p in Path('/sys/devices/system/cpu').glob('cpu[0-9]*/topology/thread_siblings_list')})
            assert len(groups)>=2,'The nested host needs two complete guest CPU groups'
            physical=os.sysconf('SC_PHYS_PAGES')*os.sysconf('SC_PAGE_SIZE');budget=(physical-4*GIB)//GIB*GIB
            assert budget>=3*GIB,'Insufficient RAM for the nested host operating floor'
            routes=json.loads(run('ip','-j','route','show'))
            import re
            exclusions=sorted({int(m[1]) for route in routes if (m:=re.fullmatch(r'10\.78\.(\d+)\.0/24',route.get('dst',''))) and 1<=int(m[1])<=200})
            c={'excluded_network_slots':exclusions,'pool':'hcnested','dataset_root':'hcnested/homecloud','image':image,'cloud_init_transport':'agent',
               'capacity':{'memory_bytes':budget,'cpu_cores':len(groups)-1,'disk_bytes':24*GIB},
               'cpu_groups':[list(g) for g in groups[1:]],'housekeeping':list(groups[0]),'allowed_gpus':[],
               'minimum_pool_available_bytes':2*GIB,'minimum_reserves':{'memory_bytes':4*GIB,'cpu_cores':1,'disk_bytes':2*GIB},
               'memory_operating_floor_bytes':2*GIB,'arc_recommended_maximum_bytes':2*GIB}
            reviewed=STATE/'initial-policy.json';reviewed.write_text(json.dumps(c));reviewed.chmod(0o600)
            stage('controller','Installing the independent Homecloud controller…')
            run('python3',str(ROOT/'scripts/install_host.py'),'--policy',str(reviewed),'--daemon',str(ROOT/'bin/homecloudd'),timeout=600)
            state.update(installed=True,stage='ready',message='Installed. Workload startup and fault recovery use the guest’s own services.')
            state['job']['phase']='succeeded';save(state)
        except Exception as e:
            state.update(message=str(e));state['job']['phase']='failed';save(state);raise


if __name__=='__main__':
    assert os.geteuid()==0
    if sys.argv[1:] == ['inspect']:print(json.dumps(inspect()))
    else:
        assert len(sys.argv)==5 and sys.argv[1]=='install'
        install(*sys.argv[2:])
