"""Startup observation and irreversible delete fences, under the host operation lock."""
from pathlib import Path
import json,os,re,subprocess,time,uuid


def live_processes(host, vm):
    name=host.vm_name(vm);unit='homecloud-'+uuid.UUID(vm['id']).hex+'.service'
    group=Path('/sys/fs/cgroup/homecloud.slice')/unit/'cgroup.procs'
    if not group.exists():return []
    found=[]
    for value in group.read_text().split():
        try:
            pid=int(value);proc=Path('/proc')/str(pid)
            args=proc.joinpath('cmdline').read_bytes().split(b'\0')
            names=[args[i+1].decode().split(',')[0] for i in range(len(args)-1) if args[i]==b'-name']
            if name in names and 'qemu-system-' in proc.joinpath('exe').resolve(strict=True).name:found.append(pid)
        except (OSError,ValueError,UnicodeError):continue  # process exited while observed
    return found


def boot_id():
    return Path('/proc/sys/kernel/random/boot_id').read_text().strip()


def wait(host, vm, stage, message, timeout):
    path=host.STATE/('startup-'+uuid.UUID(vm['id']).hex+'.json')
    boot=boot_id();now=time.monotonic()
    old=json.loads(path.read_text()) if path.exists() else {}
    if old.get('boot')!=boot or old.get('stage')!=stage:
        old={'boot':boot,'stage':stage,'since':now};host.save(path,old)
    if now-old['since']>=timeout:
        path.unlink()  # An explicit reviewed retry receives a new bounded wait.
        raise RuntimeError('conflict: '+message+' timed out; claims remain held. Retry or delete this VM after review.')
    raise RuntimeError('pending: '+message)


def launch_path(host,vm):
    return host.STATE/('launch-'+uuid.UUID(vm['id']).hex+'.json')

def launch_error_path(host,vm):
    return host.STATE/('launch-error-'+uuid.UUID(vm['id']).hex+'.json')


def setup_finished(host,vm):
    # Completion of guest setup is not the end of a launch epoch: its response
    # can be lost before the controller commits it. Keep the launch guard until
    # a confirmed stop, including across Incus-internal guest reboot gaps.
    progress_path(host,vm).unlink(missing_ok=True)
    launch_error_path(host,vm).unlink(missing_ok=True)
    path=host.STATE/('startup-'+uuid.UUID(vm['id']).hex+'.json')
    path.unlink(missing_ok=True)


def finished(host,vm):
    launch_path(host,vm).unlink(missing_ok=True)
    setup_finished(host,vm)


def cid_consistent(host,vm):
    instance=host.owned(vm)
    conf=Path('/run/incus')/host.vm_name(vm)/'qemu.conf'
    if not conf.exists():return False
    match=re.search(r'guest-cid\s*=\s*"(\d+)"',conf.read_text())
    cid=instance['config'].get('volatile.vsock_id')
    if cid and match and cid!=match.group(1):
        raise RuntimeError('conflict: Incus VSOCK identity differs from QEMU configuration; guest-agent access needs review. Claims remain held; no automatic restart was attempted.')

    return bool(cid and match)


def start(host,vm):
    power=host.state(vm)
    if power=='running':
        cid_consistent(host,vm);return
    # Incus can transiently report Stopped during startup. A second start can
    # allocate another VSOCK ID before discovering the first QEMU is alive.
    if live_processes(host,vm):wait(host,vm,'start','waiting for the existing QEMU startup',180)
    # Guest reboots are driven by Incus internally and may have no public
    # operation. A successful/ambiguous launch remains guarded until confirmed
    # shutdown, including its brief Stopped window; never race it with a start.
    attempt=launch_path(host,vm)
    if attempt.exists() and json.loads(attempt.read_text()).get('boot') and json.loads(attempt.read_text())['boot']!=boot_id():
        # Only a different kernel boot proves the old QEMU cannot still exist.
        attempt.unlink()
    if attempt.exists():
        try:wait(host,vm,'start','waiting for the previous launch or first-boot reboot',180)
        except RuntimeError as error:
            if str(error).startswith('conflict:'):attempt.unlink()
            raise
    operations=host.api('GET','/1.0/operations?recursion=1')
    target='/1.0/instances/'+host.vm_name(vm)
    for group in operations.values():
        for op in group:
            if op.get('status_code') not in (100,103):continue
            resources=op.get('resources') or {}
            if any(url.split('?',1)[0]==target for url in resources.get('instances',[])):
                wait(host,vm,'start','waiting for the existing Incus operation',180)
    checkpoint(host,vm,'starting')
    host.save(attempt,{'boot':boot_id()})
    fd=os.open(host.STATE,os.O_RDONLY|os.O_DIRECTORY)
    try:os.fsync(fd)
    finally:os.close(fd)
    try:p=host.run('incus','start',host.vm_name(vm),check=False,timeout=120)
    except subprocess.TimeoutExpired:
        wait(host,vm,'start','waiting for the interrupted Incus start acknowledgement',180)
    power=host.state(vm)
    if power=='running':cid_consistent(host,vm);return
    if live_processes(host,vm):wait(host,vm,'start','waiting for the existing QEMU startup',180)
    if p.returncode:
        # Preserve the original command failure while subsequent observations
        # wait conservatively for an ambiguous launch. Do not replace it with
        # a generic timeout and lose the only useful diagnostic.
        host.save(launch_error_path(host,vm),{'boot':boot_id(),'command_error':p.stderr[-4096:]})
        raise RuntimeError(p.stderr)
    wait(host,vm,'start','waiting for Incus to report the VM running',180)


def fence(host,vm):
    """A delayed create/start must never resurrect an explicitly deleted VM ID."""
    path=host.STATE/('deleted-intent-'+uuid.UUID(vm['id']).hex+'.json')
    expected={'id':vm['id'],'owner':vm['owner']}
    if path.exists():
        assert json.loads(path.read_text())==expected,'conflict: deletion fence ownership differs'
        assert vm['desired']=='deleted','conflict: VM deletion was requested; stale apply rejected'
    elif vm['desired']=='deleted':
        host.save(path,expected)
        fd=os.open(host.STATE,os.O_RDONLY|os.O_DIRECTORY)
        try:os.fsync(fd)
        finally:os.close(fd)


STAGES={'creating','starting','guest_agent','guest_setup','gpu_driver','ssh_identity'}

def progress_path(host,vm):return host.STATE/('setup-'+uuid.UUID(vm['id']).hex+'.json')

def progress(host,vm):
    path=progress_path(host,vm)
    return json.loads(path.read_text()) if path.exists() else None

def checkpoint(host,vm,stage):
    assert stage in STAGES
    old=progress(host,vm)
    if not old or old['stage']!=stage:
        host.save(progress_path(host,vm),{'stage':stage,'since':int(time.time())})

def diagnostics(host,vm):
    # Fixed commands/files only. Authorization is checked again by owned().
    d=host.owned(vm,missing=True);checks=[];logs=[]
    power=host.state(vm) if d is not None else 'missing'
    failure=launch_error_path(host,vm)
    if failure.exists():
        detail=json.loads(failure.read_text()).get('command_error','')[:4096]
        if detail:checks.append('Last Incus start error: '+detail)
    if d is None:checks.append('The VM has not been created in Incus, or is missing. Existing claims remain protected.')
    else:
        try:
            checks.append('Incus and QEMU VSOCK configuration agree.' if cid_consistent(host,vm) else 'VSOCK configuration is not available for comparison yet.')
        except RuntimeError as error:checks.append(str(error))
        if power=='stopped' and live_processes(host,vm):checks.append('QEMU is still alive while Incus reports stopped. Claims remain held; administrator review is required.')
    if power=='running':
        code=r"""import json,os,stat
files=['/var/log/cloud-init-output.log','/var/log/cloud-init.log'];logs=[]
for name in files:
 try:
  fd=os.open(name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
  try:
   assert stat.S_ISREG(os.fstat(fd).st_mode),'Not a regular log file'
   size=os.fstat(fd).st_size;os.lseek(fd,max(0,size-12000),os.SEEK_SET)
   text=os.read(fd,12000).decode('utf-8','replace')
   logs.append({'name':name,'text':('[Earlier output omitted]\n' if size>12000 else '')+text})
  finally:os.close(fd)
 except (OSError,AssertionError):logs.append({'name':name,'text':'Log unavailable'})
print(json.dumps(logs))"""
        try:
            p=host.run('incus','exec',host.vm_name(vm),'--','python3','-c',code,check=False,timeout=8)
            if p.returncode==0 and len(p.stdout)<=100000:logs=json.loads(p.stdout)
            else:checks.append('Guest setup logs could not be read. Refresh, or ask an administrator to inspect guest setup.')
        except (subprocess.TimeoutExpired,ValueError):checks.append('Guest agent did not return setup logs within eight seconds. Try refreshing after it starts.')
    else:checks.append('Guest logs require a running VM and a responding guest agent.')
    return {'vm_id':vm['id'],'progress':progress(host,vm),'power_state':power,'checks':checks,'logs':logs}
