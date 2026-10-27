"""Root-side bounded storage exhaustion proof using real Incus VM roots.

The only vdev is a <=32 GiB file in a quota-limited dataset on the authorized
lab disk. Never exhaust/export/reformat the real host pool.
"""
import errno
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import traceback

G = 1024**3
POOL = 'hcproof'
BACKING = 'hclab/proof-backing'
MOUNT = Path('/var/lib/home-cloud-lab/proof-backing')
WORK = Path('/var/lib/home-cloud-lab/storage-proof')
EVIDENCE = Path('/var/lib/home-cloud-lab/storage-guarantees.json')
IMAGE = '2139edd9da4026da8a66c0557af7eee4789101104eb27860bacd57086219cebb'
TAG = 'storage-v1'
report = {'passed': False, 'stages': {}, 'started_at': time.time(),
          'guest_virtual_bytes_each': 10*G, 'committed_backing_bytes_each': 12*G,
          'test_pool_bytes': 32*G, 'system_reserve_bytes': 2*G,
          'minimum_unreserved_accounting_headroom_bytes': G,
          'fixture': 'file-backed ZFS pool; real Incus VM root zvols; not a performance or disk-failure test'}


def run(*args, check=True, timeout=120):
    p = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if check and p.returncode:
        raise RuntimeError(f'{args}: {p.stdout}\n{p.stderr}')
    return p


def stage(name, data):
    report['stages'][name] = data
    EVIDENCE.write_text(json.dumps(report, indent=2) + '\n')
    print(name, flush=True)


def accounting():
    run('zpool', 'sync', POOL)
    return run('zfs', 'list', '-p', '-r', '-o',
               'name,type,used,available,quota,reservation,refreservation,volsize,origin', POOL).stdout


def fill(path, limit, block_bytes=1024*1024):
    written = 0
    error = None
    with open(path, 'wb', buffering=0) as f:
        while written < limit:
            try:
                block = os.urandom(min(block_bytes, limit-written))
                n = f.write(block)
                written += n
                if written % (16*1024*1024) == 0: os.fsync(f.fileno())
            except OSError as e:
                assert e.errno in (errno.ENOSPC, errno.EDQUOT), e
                error = e.errno
                break
        try: os.fsync(f.fileno())
        except OSError as e:
            assert e.errno in (errno.ENOSPC, errno.EDQUOT), e
            error = e.errno
    return {'bytes_written': written, 'errno': error}


FILL_GUEST = r'''
import errno,hashlib,json,os
p='/var/tmp/guarantee-proof.bin'
n=0;first=None;last=None;error=None;tail=b''
initial_free=os.statvfs('/').f_bfree*os.statvfs('/').f_frsize
with open(p,'wb',buffering=0) as f:
 while n < 11*1024**3:
  b=os.urandom(1024*1024)
  try:
   count=f.write(b)
   if first is None:first=hashlib.sha256(b[:count]).hexdigest()
   tail=(tail+b[:count])[-1048576:]
   last=hashlib.sha256(tail).hexdigest();n+=count
   if count != len(b):error=errno.ENOSPC;break
   if n%(16*1024*1024)==0:os.fsync(f.fileno())
  except OSError as e:
   assert e.errno in (errno.ENOSPC,errno.EDQUOT),e
   error=e.errno;break
 try:os.fsync(f.fileno())
 except OSError as e:
  assert e.errno in (errno.ENOSPC,errno.EDQUOT),e
  error=e.errno
with open(p,'rb') as f:
 actual_first=hashlib.sha256(f.read(1024*1024)).hexdigest()
 f.seek(n-1024*1024);actual_last=hashlib.sha256(f.read(1024*1024)).hexdigest()
assert actual_first==first and actual_last==last
print(json.dumps({'bytes_written':n,'errno':error,'first_sha256':first,'last_sha256':last,'readback_correct':True,'initial_free_bytes':initial_free,'guest_free_bytes':os.statvfs('/').f_bfree*os.statvfs('/').f_frsize,'guest_available_bytes':os.statvfs('/').f_bavail*os.statvfs('/').f_frsize}))
'''


def guest(vm, *args, timeout=120):
    return run('incus', 'exec', vm, '--', *args, timeout=timeout).stdout


def wait_agent(vm):
    end = time.monotonic()+180
    while time.monotonic()<end:
        if run('incus','exec',vm,'--','true',check=False,timeout=10).returncode==0:return
        time.sleep(2)
    raise RuntimeError(f'{vm}: agent unavailable')


def fill_guest(vm):
    p=subprocess.Popen(['incus','exec',vm,'--','python3','-c',FILL_GUEST],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    deadline=time.monotonic()+1200
    try:
        while time.monotonic()<deadline:
            try:
                out,err=p.communicate(timeout=5)
                if p.returncode:raise RuntimeError(f'{vm}: {out} {err}')
                return json.loads(out)
            except subprocess.TimeoutExpired:
                state=json.loads(run('incus','query',f'/1.0/instances/{vm}/state').stdout)
                if state['status']=='Error':
                    log=Path(f'/var/log/incus/{vm}/qemu.qmp.log').read_text()
                    report['qemu_io_errors']=[l for l in log.splitlines() if 'BLOCK_IO_ERROR' in l]
                    raise RuntimeError(f'{vm}: QEMU I/O error while filling guest disk')
        raise TimeoutError(vm+' disk-fill timeout')
    finally:
        if p.poll() is None:p.terminate();p.wait(timeout=10)


assert os.geteuid()==0
assert run('zpool','list','-H','-o','health','hclab').stdout.strip()=='ONLINE'
assert int(run('zfs','get','-Hp','-o','value','available','hclab').stdout)>100*G
assert run('zpool','list',POOL,check=False).returncode != 0, 'Existing proof pool requires inspection'
assert run('zfs','list',BACKING,check=False).returncode != 0, 'Existing backing requires inspection'
if MOUNT.exists():
    assert not os.path.ismount(MOUNT) and not list(MOUNT.iterdir()), 'Unexpected backing directory content'
    MOUNT.rmdir()
WORK.mkdir(parents=True,exist_ok=True)
created_pool=False
try:
    run('zfs','create','-o',f'mountpoint={MOUNT}','-o','canmount=on','-o','compression=off',
        '-o','quota=36G','-o',f'org.homecloud:proof={TAG}',BACKING)
    vdev=MOUNT/'vdev.bin'
    with vdev.open('wb') as f:f.truncate(32*G)
    run('zpool','create','-o','ashift=12','-o','cachefile=none','-O','compression=off',
        '-O','atime=off','-O','canmount=off','-m','none',POOL,str(vdev))
    created_pool=True
    run('zfs','create','-o','reservation=2G',POOL+'/system-reserve')
    for user,cpus in [('a','4,10'),('b','5,11')]:
        tenant=f'{POOL}/{user}';pool=f'hclab-proof-{user}';vm=f'hclab-proof-storage-{user}'
        assert run('incus','query',f'/1.0/instances/{vm}',check=False).returncode!=0
        run('zfs','create','-o','reservation=12G','-o','quota=12G',tenant)
        run('zfs','create',tenant+'/incus')
        run('incus','storage','create',pool,'zfs',f'source={tenant}/incus',
            'volume.zfs.reserve_space=false','volume.zfs.use_refquota=false')
        # No network or package installation is needed for storage fixtures.
        run('incus','init',IMAGE,vm,'--vm','--no-profiles','-s',pool,
            '-c','limits.memory=2GiB','-c',f'limits.cpu={cpus}',
            '-c',f'user.home-cloud-proof={TAG}','-c','boot.autostart=false',timeout=300)
        run('incus','start',vm)
        wait_agent(vm)
    stage('initial_layout',accounting())
    # Exercise Incus-managed block snapshots/clones and deferred deletion.
    apool='hclab-proof-a'
    run('incus','storage','volume','create',apool,'churn','--type=block','size=64MiB')
    vm='hclab-proof-storage-a'
    run('incus','config','device','add',vm,'churn','disk',f'pool={apool}','source=churn')
    def small_disk(vm):
        disks=json.loads(guest(vm,'lsblk','--json','--bytes','-o','NAME,SIZE,TYPE'))['blockdevices']
        matches=[x for x in disks if int(x['size'])==64*1024*1024 and x['type']=='disk']
        assert len(matches)==1,disks
        return '/dev/'+matches[0]['name']
    device=small_disk(vm)
    code='import hashlib,os,sys;p=os.urandom(67108864);f=open(sys.argv[1],"wb",buffering=0);assert f.write(p)==len(p);os.fsync(f.fileno());f.close();print(hashlib.sha256(p).hexdigest())'
    expected=guest(vm,'python3','-c',code,device).strip()
    run('incus','config','device','remove',vm,'churn')
    run('incus','storage','volume','snapshot','create',apool,'churn','s0')
    run('incus','storage','volume','copy',apool+'/churn/s0',apool+'/churn-copy')
    run('incus','storage','volume','delete',apool,'churn')
    run('incus','config','device','add',vm,'churn-copy','disk',f'pool={apool}','source=churn-copy')
    device=small_disk(vm)
    code='import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())'
    assert guest(vm,'python3','-c',code,device).strip()==expected
    stage('clone_survives_origin_deletion',{'readback_correct':True,'sha256':expected,'accounting':accounting()})
    run('incus','config','device','remove',vm,'churn-copy')
    run('incus','storage','volume','delete',apool,'churn-copy')
    # A consumes its entire guest disk. Then charge further activity to its
    # parent until the tenant cap prevents additional physical consumption.
    stage('a_guest_disk_full',fill_guest('hclab-proof-storage-a'))
    run('zfs','create','-o','canmount=on','-o',f'mountpoint={WORK}/a-pressure',POOL+'/a/pressure')
    pressure_a=fill(WORK/'a-pressure'/'fill.bin',13*G)
    assert pressure_a['errno'] in (errno.ENOSPC,errno.EDQUOT),pressure_a
    stage('a_parent_cap_enforced',{'fill':pressure_a,'accounting':accounting()})
    run('zfs','create','-o','canmount=on','-o',f'mountpoint={WORK}/global-pressure',POOL+'/pressure')
    available=int(run('zfs','get','-Hp','-o','value','available',POOL).stdout)
    pressure_cap=available-G-128*1024*1024
    assert pressure_cap>G
    run('zfs','set',f'quota={pressure_cap}',POOL+'/pressure')
    pressure_global=fill(WORK/'global-pressure'/'fill.bin',33*G)
    assert pressure_global['errno'] in (errno.ENOSPC,errno.EDQUOT),pressure_global
    before_b=accounting()
    headroom=int(run('zfs','get','-Hp','-o','value','available',POOL).stdout)
    assert headroom>=G,headroom
    stage('all_admitted_non_tenant_capacity_exhausted',{'fill':pressure_global,'unreserved_headroom':headroom,'accounting':before_b})
    b=fill_guest('hclab-proof-storage-b')
    a=report['stages']['a_guest_disk_full']
    assert b['readback_correct'] and b['errno']==errno.ENOSPC,b
    assert b['bytes_written']>8*G and b['bytes_written']>=b['initial_free_bytes']-64*1024*1024,b
    assert b['guest_free_bytes']<=64*1024*1024,b
    stage('b_full_disk_after_a_and_pool_exhaustion',{'fill':b,'accounting':accounting()})
    # Verify committed backing survives a lower admission target; the target
    # is not translated into a smaller ZFS reservation or hard cap.
    commitment=run('zfs','get','-Hp','-o','value','reservation',POOL+'/b').stdout.strip()
    assert int(commitment)==12*G
    extra=run('zfs','create','-o','reservation=2G',POOL+'/unbacked-extra',check=False)
    assert extra.returncode!=0, 'Existing commitments became allocatable'
    stage('lower_admission_target_retains_backing',{
        'example_admission_target_bytes':6*G,'existing_commitment_bytes':int(commitment),
        'extra_reservation_rejected':True,'note':'Storage primitive only; controller transaction/admission implementation remains required'})
    (WORK/'a-pressure'/'fill.bin').unlink()
    # Start/restart a full guest without losing previously written data.
    before_boot=guest('hclab-proof-storage-b','cat','/proc/sys/kernel/random/boot_id')
    run('incus','restart','hclab-proof-storage-b','--timeout','30',timeout=180)
    wait_agent('hclab-proof-storage-b')
    assert guest('hclab-proof-storage-b','cat','/proc/sys/kernel/random/boot_id')!=before_boot
    verify_code='import hashlib,json;f=open("/var/tmp/guarantee-proof.bin","rb");print(hashlib.sha256(f.read(1048576)).hexdigest())'
    assert guest('hclab-proof-storage-b','python3','-c',verify_code).strip()==b['first_sha256']
    used_before=int(run('zfs','get','-Hp','-o','value','used',POOL+'/b').stdout)
    guest('hclab-proof-storage-b','rm','/var/tmp/guarantee-proof.bin')
    trim=guest('hclab-proof-storage-b','fstrim','-v','/')
    time.sleep(3)
    after=accounting();used_after=int(run('zfs','get','-Hp','-o','value','used',POOL+'/b').stdout)
    assert used_before-used_after>8*G,(used_before,used_after,trim)
    assert int(run('zfs','get','-Hp','-o','value','reservation',POOL+'/b').stdout)==12*G
    stage('restart_delete_and_trim',{'data_survived_restart':True,'trim':trim,
          'used_before':used_before,'used_after':used_after,'reservation_retained':True,'accounting':after})
    # Export/import just the scratch pool after shutting down its two guests.
    for user in ('a','b'):
        vm=f'hclab-proof-storage-{user}'
        p=run('incus','stop',vm,'--timeout','30',check=False,timeout=90)
        if p.returncode:run('incus','stop',vm,'--force')
    run('zpool','export',POOL)
    run('zpool','import','-d',str(MOUNT),'-o','cachefile=none',POOL)
    assert int(run('zfs','get','-Hp','-o','value','reservation',POOL+'/b').stdout)==12*G
    run('incus','start','hclab-proof-storage-b');wait_agent('hclab-proof-storage-b')
    stage('scratch_pool_reimport',{'reservation_retained':True,'guest_booted':True,'health':run('zpool','status','-x',POOL).stdout})
    report['passed']=True
except Exception:
    report['error']=traceback.format_exc()
    if created_pool:report['failure_accounting']=accounting()
    raise
finally:
    EVIDENCE.write_text(json.dumps(report,indent=2)+'\n')
    cleanup=[]
    for user in ('a','b'):
        vm=f'hclab-proof-storage-{user}';pool=f'hclab-proof-{user}'
        p=run('incus','query',f'/1.0/instances/{vm}',check=False)
        if p.returncode==0:
            d=json.loads(p.stdout);assert d['config'].get('user.home-cloud-proof')==TAG
            p=run('incus','stop',vm,'--timeout','30',check=False,timeout=90)
            if p.returncode:run('incus','stop',vm,'--force',check=False)
            run('incus','delete',vm)
        p=run('incus','storage','show',pool,check=False)
        if p.returncode==0:
            # Remove any custom fixtures left by a failed clone stage.
            for volume in ('churn-copy','churn'):run('incus','storage','volume','delete',pool,volume,check=False)
            run('incus','storage','delete',pool)
        cleanup.append(vm)
    if created_pool:
        assert str(MOUNT/'vdev.bin') in run('zpool','status','-P',POOL).stdout
        run('zpool','destroy',POOL)
    if run('zfs','list',BACKING,check=False).returncode==0:
        assert run('zfs','get','-H','-o','value','org.homecloud:proof',BACKING).stdout.strip()==TAG
        run('zfs','destroy',BACKING) if not (MOUNT/'vdev.bin').exists() else None
        if (MOUNT/'vdev.bin').exists():
            (MOUNT/'vdev.bin').unlink();run('zfs','destroy',BACKING)
    report['cleanup']=cleanup;report['completed_at']=time.time()
    report['host_pool_health']=run('zpool','status','-x','hclab').stdout
    EVIDENCE.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({k:v for k,v in report.items() if k!='stages'},indent=2),flush=True)
