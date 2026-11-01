#!/usr/bin/python3
"""Fixed-command privileged adapter. No listener and no arbitrary shell RPC.

Install root-owned at /usr/local/libexec/homecloud/host_runtime.py. Network
clients cannot supply paths, commands, image URLs, networks, or host policies.
"""
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
import managed_apps
import managed_host
import memory_capacity
import resource_reserves
import instance_runtime

CONFIG = Path('/etc/homecloud/host.json')
STATE = Path('/var/lib/homecloud/host')
GIB = 1024**3


def run(*args, check=True, timeout=90, input=None):
    p = subprocess.run(args, input=input, capture_output=True, text=True, timeout=timeout)
    if check and p.returncode:
        raise RuntimeError(f'{args[0]} {args[1] if len(args)>1 else ""}: {p.stderr.strip() or p.stdout.strip()}')
    return p


def api(method, path, data=None, missing=False):
    args = ['incus', 'query', '-X', method, path]
    if data is not None: args += ['-d', json.dumps(data)]
    if method != 'GET': args += ['--wait']
    p = run(*args, check=not missing, timeout=8 if method == 'GET' else 90)
    if p.returncode:
        if missing and ('not found' in p.stderr.lower() or 'not found' in p.stdout.lower()): return None
        raise RuntimeError(p.stderr or p.stdout)
    return json.loads(p.stdout) if p.stdout.strip() else {}


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_suffix('.tmp')
    with temp.open('w') as f:
        json.dump(value, f, indent=2); f.flush(); os.fsync(f.fileno())
    temp.chmod(0o600); temp.replace(path)


def text(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists() and path.read_text() == value: return False
    temporary = path.with_suffix('.tmp')
    temporary.write_text(value); temporary.chmod(0o644); temporary.replace(path)
    return True


def load_config():
    st = CONFIG.stat()
    assert st.st_uid == 0 and not st.st_mode & 0o022, 'Host config must be owned and writable only by root'
    c = json.loads(CONFIG.read_text())
    assert re.fullmatch(r'[A-Za-z0-9_-]+', c['pool'])
    assert c['dataset_root'] == c['pool'] + '/homecloud'
    assert re.fullmatch(r'[a-f0-9]{64}', c['image'])
    assert all(type(slot) is int and 1<=slot<=200 for slot in c.get('excluded_network_slots',[]))
    if c.get('managed_host_image'):assert re.fullmatch(r'[a-f0-9]{64}',c['managed_host_image'])
    assert c.get('cloud_init_transport','agent') in ('agent','disk'), 'Review the pinned image cloud-init transport'
    groups = c['cpu_groups']; house = c['housekeeping']
    assert groups and house and all(g for g in groups)
    cpus = house + [x for g in groups for x in g] + [x for v in c.get('legacy_vms', []) for x in v['cpu_ids']]
    assert len(cpus) == len(set(cpus)) and all(isinstance(x, int) and 0 <= x < 1024 for x in cpus)
    for group in groups:
        siblings = Path(f'/sys/devices/system/cpu/cpu{group[0]}/topology/thread_siblings_list').read_text().strip()
        assert parse_cpus(siblings) == set(group), 'CPU allocation splits a physical core'
    c=memory_capacity.overlay(sys.modules[__name__],c)
    return resource_reserves.effective(c, resource_reserves.read_record(sys.modules[__name__], c))


def parse_cpus(value):
    result = set()
    for part in value.strip().split(','):
        if not part: continue
        ends = [int(x) for x in part.split('-')]
        result.update(range(ends[0], ends[-1] + 1))
    return result


def uid(value): return str(uuid.UUID(value))
def vm_name(vm): return 'hc-' + uuid.UUID(vm['id']).hex
def tenant(c, user): return c['dataset_root'] + '/users/' + uuid.UUID(user).hex
def storage_pool(user): return 'hc-u-' + uuid.UUID(user).hex
def cpulist(cpus): return ','.join(str(x) for x in sorted(set(cpus)))
def zget(dataset, prop): return int(run('zfs', 'get', '-Hp', '-o', 'value', prop, dataset).stdout)


def dataset(name, **properties):
    if run('zfs', 'list', '-H', name, check=False).returncode:
        args = ['zfs', 'create', '-p']
        for key, value in properties.items(): args += ['-o', f'{key}={value}']
        run(*args, name)


def owned(vm, missing=False):
    d = api('GET', '/1.0/instances/' + vm_name(vm), missing=missing)
    if d is None: return None
    assert d['config'].get('user.homecloud.id') == uid(vm['id']), 'conflict: instance ID tag mismatch'
    assert d['config'].get('user.homecloud.owner') == uid(vm['owner']), 'conflict: instance ownership mismatch'
    return d


def state(vm): return api('GET', '/1.0/instances/' + vm_name(vm) + '/state')['status'].lower()


def stop(vm, force=False):
    if state(vm) == 'stopped':
        assert not instance_runtime.live_processes(sys.modules[__name__],vm),'conflict: QEMU is still alive despite a stopped Incus state; claims remain held'
        instance_runtime.finished(sys.modules[__name__],vm)
        return
    args = ['--force'] if force else ['--timeout', '30']
    try:
        run('incus', 'stop', vm_name(vm), *args, check=False, timeout=60)
    except subprocess.TimeoutExpired:
        # A timed-out client is not proof that the hypervisor has stopped.
        # Probe before deciding; force is never an automatic fallback.
        pass
    if state(vm) != 'stopped' or instance_runtime.live_processes(sys.modules[__name__],vm):
        raise RuntimeError('conflict: force stop did not complete; resources remain held' if force else 'conflict: graceful shutdown did not complete; keep waiting or explicitly force stop')


    instance_runtime.finished(sys.modules[__name__],vm)


def shutdown_boot():
    return str(uuid.UUID(Path('/proc/sys/kernel/random/boot_id').read_text().strip()))


def shutdown_fence_path(operation_id):
    return STATE / ('shutdown-cancelled-' + uuid.UUID(operation_id).hex + '.json')


def check_shutdown_fence(operation_id, boot_id):
    assert boot_id == shutdown_boot(), 'Host restarted; request a new shutdown'
    assert not shutdown_fence_path(operation_id).exists(), 'Shutdown cancellation is already fenced on the host; finish cancellation to resume VMs'
    return {'kind':'ok'}


def cancel_shutdown(operation_id, boot_id):
    assert boot_id == shutdown_boot(), 'Host restarted; request a new shutdown'
    save(shutdown_fence_path(operation_id), {'operation_id':uid(operation_id),'boot_id':boot_id})
    fd=os.open(STATE,os.O_RDONLY|os.O_DIRECTORY)
    try:os.fsync(fd)
    finally:os.close(fd)
    return {'kind':'ok'}


def drain_guest(operation_id, vm, boot_id):
    check_shutdown_fence(operation_id, boot_id)
    assert owned(vm, missing=True) is not None, 'Recorded VM instance is missing. Cancel host shutdown and recover or delete this VM before trying again.'
    # Host shutdown retains Running intent, GPU devices, CPU policy and claims.
    try:stop(vm)
    except RuntimeError as error:
        if str(error).startswith('conflict: graceful shutdown did not complete'):
            raise RuntimeError('Graceful guest shutdown did not complete. The host remains on. Retry host shutdown, or cancel it and recover this VM before trying again.') from None
        raise
    return {'kind':'ok'}


def check_shutdown(boot_id, vms):
    assert boot_id == shutdown_boot(), 'Host restarted; request a new shutdown'
    # Include unmanaged/frozen guests: the OS must not silently force them off.
    actual = api('GET', '/1.0/instances?recursion=1&all-projects=true')
    assert all(v['status'].lower() == 'stopped' for v in actual), 'A guest is still active; host remains on'
    for vm in vms:
        owned(vm)
        assert state(vm) == 'stopped' and not instance_runtime.live_processes(sys.modules[__name__], vm), 'Guest shutdown is not confirmed; host remains on'
    return {'kind':'ok'}


def power_off(operation_id, boot_id, vms):
    operation_id=uid(operation_id)
    try:
        check_shutdown_fence(operation_id, boot_id)
        check_shutdown(boot_id, vms)
    except Exception as error:
        # No OS shutdown command ran. A matching acknowledged rejection can
        # restore cancellation, unless an earlier attempt is still ambiguous.
        return {'kind':'power_off_rejected','data':{'operation_id':operation_id,'boot_id':boot_id,'message':str(error)}}
    run('systemctl', 'poweroff', timeout=15)
    return {'kind':'power_off_queued','data':{'operation_id':operation_id,'boot_id':boot_id}}


def start(vm):
    return instance_runtime.start(sys.modules[__name__],vm)


def exec_guest(vm, *args, check=True, timeout=15):
    return run('incus', 'exec', vm_name(vm), '--', *args, check=check, timeout=timeout)


def backing(c, users):
    root = c['dataset_root']
    dataset(root, mountpoint='none', canmount='off')
    dataset(root + '/users', mountpoint='none', canmount='off')
    total = sum(max(u['grant']['disk_bytes'], u['committed']['disk_bytes']) for u in users)
    assert total <= c['capacity']['disk_bytes'], 'conflict: backing exceeds host-managed budget'
    for user in users:
        target = max(user['grant']['disk_bytes'], user['committed']['disk_bytes'])
        parent = tenant(c, user['id'])
        exists = run('zfs', 'list', '-H', parent, check=False).returncode == 0
        if target == 0 and not exists: continue
        if not exists:
            dataset(parent, mountpoint='none', canmount='off')
            run('zfs','set','org.homecloud:owner='+uid(user['id']),parent)
        else:
            assert run('zfs','get','-H','-o','value','org.homecloud:owner',parent).stdout.strip()==uid(user['id']),'conflict: foreign tenant dataset'
        old = zget(parent, 'reservation')
        floor = c['minimum_pool_available_bytes']
        if target > old:
            assert zget(c['pool'], 'available') >= target - old + floor, 'pending: pool operating floor prevents further backing'
            run('zfs', 'set', f'quota={target}', parent)
            run('zfs', 'set', f'reservation={target}', parent)
        elif target < old:
            # Existing logical commitments are included in target; never derive
            # reclamation directly from a reduced admission ceiling.
            assert target >= user['committed']['disk_bytes']
            run('zfs', 'set', f'reservation={target}', parent)
            run('zfs', 'set', 'quota=' + (str(target) if target else 'none'), parent)
        assert zget(c['pool'], 'available') >= floor, 'pending: ZFS accounting floor below policy'
        if target:
            dataset(parent + '/incus', mountpoint='none', canmount='off')
            name = storage_pool(user['id'])
            pool = api('GET', '/1.0/storage-pools/' + name, missing=True)
            if pool is None:
                api('POST', '/1.0/storage-pools', {'name':name,'driver':'zfs','config':{'source':parent+'/incus','volume.zfs.reserve_space':'false','volume.zfs.use_refquota':'false'}})
            else: assert pool['config'].get('source') == parent + '/incus', 'conflict: storage pool source changed'


def cpu_layout(c):
    house = set(c['housekeeping']); mask = f'{sum(1 << x for x in house):x}'
    os.sched_setaffinity(1, house)
    Path('/proc/irq/default_smp_affinity').write_text(mask)
    for p in Path('/proc/irq').glob('*/smp_affinity_list'):
        try: p.write_text(cpulist(house))
        except OSError: pass  # managed IRQs may be immovable; inventory reports scope
    for p in Path('/sys/devices/virtual/workqueue').glob('**/cpumask'):
        try: p.write_text(mask)
        except OSError: pass


def cpu_policy(c, vms, activate=True):
    house = c['housekeeping']; entries = list(c.get('legacy_vms', []))
    for vm in vms:
        if vm['observed'] == 'deleted': continue
        if not vm['cpu_ids']:
            assert vm['desired'] in ('stopped','deleted') and vm['resources']['cpu_cores']==0, 'conflict: running VM has no CPU claim'
            continue
        uid(vm['id']); uid(vm['owner'])
        assert vm['cpu_ids'] and all(x in [i for g in c['cpu_groups'] for i in g] for x in vm['cpu_ids'])
        assert all(not (set(g) & set(vm['cpu_ids'])) or set(g) <= set(vm['cpu_ids']) for g in c['cpu_groups']), 'conflict: split SMT core'
        entries.append({'name':vm_name(vm),'cpu_ids':vm['cpu_ids'],'unit':'homecloud-' + uuid.UUID(vm['id']).hex + '.service'})
    occupied = set()
    for entry in entries:
        assert not occupied.intersection(entry['cpu_ids']), 'conflict: overlapping CPU assignments'
        occupied.update(entry['cpu_ids'])
    lines = []; changed = False
    for entry in entries:
        assert re.fullmatch(r'(hc-[a-f0-9]{32}|hclab-vm0[12])', entry['name'])
        unit = entry['unit']
        assert re.fullmatch(r'homecloud-(vm0[12]|[a-f0-9]{32})\.service', unit)
        allowed = cpulist(house + entry['cpu_ids'])
        path = '/sys/fs/cgroup/homecloud.slice/' + unit
        lines.append(f'{entry["name"]}\t{allowed}\t{cpulist(house)}\t{path}\n')
        file = Path('/etc/systemd/system') / unit
        if not file.exists():
            changed |= text(file, f'[Unit]\nBefore=incus.service\n[Service]\nType=simple\nExecStart=/usr/bin/sleep infinity\nDelegate=yes\nSlice=homecloud.slice\nAllowedCPUs={allowed}\nCPUAffinity={cpulist(house).replace(",", " ")}\n')
        changed |= text(Path('/etc/systemd/system')/(unit+'.d')/'homecloud-policy.conf',f'[Service]\nAllowedCPUs=\nAllowedCPUs={allowed}\nCPUAffinity=\nCPUAffinity={cpulist(house).replace(",", " ")}\n')
    changed |= text(Path('/etc/homecloud/cpu-policy.tsv'), ''.join(lines))
    if changed: run('systemctl','daemon-reload')
    for entry in entries:
        if activate: run('systemctl','start',entry['unit'])
        run('systemctl','set-property','--runtime',entry['unit'],'AllowedCPUs='+cpulist(house+entry['cpu_ids']))
    save(STATE/'cpu-vms.json',vms)


def apparmor(vm):
    unit='homecloud-' + uuid.UUID(vm['id']).hex + '.service'
    return f'''/usr/bin/qemu-system-x86_64 rix,
/etc/homecloud/cpu-policy.tsv r,
/sys/fs/cgroup/homecloud.slice/{unit}/cgroup.procs w,
/sys/fs/cgroup/homecloud.slice/{unit}/cpuset.cpus.effective r,
/var/log/homecloud-cpu-launch.jsonl w,
'''


def cloud_init(key,t3_version=None):
    assert isinstance(key,str) and len(key)<8192 and not any(x in key for x in '\n\r')
    ssh='UsePAM yes\nPubkeyAuthentication yes\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPermitRootLogin no\nAuthorizedKeysFile .ssh/authorized_keys\nSubsystem sftp internal-sftp\nInclude /etc/ssh/sshd_config.d/*.conf\n'
    config={'users':[{'name':'dev','groups':['sudo'],'sudo':'ALL=(ALL) NOPASSWD:ALL','shell':'/bin/bash','lock_passwd':True,'ssh_authorized_keys':[key]}],
            'ssh_pwauth':False,'disable_root':True,'ssh_deletekeys':True,'package_update':True,
            'packages':['openssh-server','curl','git','nano','ca-certificates'],
            'write_files':[{'path':'/etc/ssh/sshd_config','owner':'root:root','permissions':'0644','content':ssh},
                           {'path':'/etc/apt/apt.conf.d/80-homecloud-retries','owner':'root:root','permissions':'0644','content':'Acquire::Retries "3";\nAcquire::ForceIPv4 "true";\n'}],
            'runcmd':[['install','-d','-m','0755','/run/sshd'],['sshd','-t'],['systemctl','enable','--now','ssh'],['systemctl','restart','ssh'],['loginctl','enable-linger','dev']]}
    if t3_version:
        assert re.fullmatch(r'[0-9A-Za-z.+-]{1,80}',t3_version),'Invalid pinned T3 version'
        # T3's prebuilt Linux runtime links libatomic; Ubuntu's minimal cloud
        # image omits it. Install prerequisites before any user setup runs.
        config['packages'].append('libatomic1')
    return '#cloud-config\n'+json.dumps(config)


def seed_devices(config):
    # images: */cloud uses agent templates and its own first-boot reboot. An
    # additional NoCloud disk lets cloud-init race those templates and lose its
    # network state. A reviewed disk-only image must explicitly opt in instead.
    transport=config.get('cloud_init_transport','agent')
    assert transport in ('agent','disk'),'Unknown cloud-init transport'
    return {} if transport=='agent' else {'cloud-init':{'type':'disk','source':'cloud-init:config'}}


def release_cpu_policy(c, vm, vms):
    unit='homecloud-'+uuid.UUID(vm['id']).hex+'.service'
    group=Path('/sys/fs/cgroup/homecloud.slice')/unit
    if group.exists():
        main=int(run('systemctl','show',unit,'--property=MainPID','--value').stdout.strip() or '0')
        for procs in group.rglob('cgroup.procs'):
            assert all(int(pid)==main for pid in procs.read_text().split()), 'conflict: VM host processes remain; CPU claim retained'
    cpu_policy(c,[v for v in vms if v['id']!=vm['id']])
    run('systemctl','stop',unit)


def vm_gpus(vm, field='gpus'):
    legacy={'gpus':'gpu','preferred_gpus':'preferred_gpu','pending_gpus':'pending_gpu'}[field]
    values=vm[field] if field in vm else ([vm[legacy]] if vm.get(legacy) else [])
    return sorted({address.lower() for address in values})


def gpu_claims(vm):
    return set(vm_gpus(vm)) | set(vm_gpus(vm,'pending_gpus'))


def gpu_devices(instance):
    result={name:dev for name,dev in instance['devices'].items() if dev.get('type')=='gpu'}
    assert all(re.fullmatch(r'gpu[0-9]+',name) and dev.get('pci') and dev.get('gputype','physical')=='physical' for name,dev in result.items()), 'conflict: unrecognized GPU configuration'
    return result


def checked_gpu_devices(vm, instance):
    result=gpu_devices(instance)
    assert len({dev['pci'].lower() for dev in result.values()})==len(result), 'conflict: duplicate GPU assignment'
    inherited={name:dev for name,dev in instance.get('expanded_devices',instance['devices']).items() if dev.get('type')=='gpu'}
    assert inherited==result, 'conflict: inherited GPU configuration'
    assert {dev['pci'].lower() for dev in result.values()} <= gpu_claims(vm), 'conflict: GPU assignment drift'
    return result


def target_gpus(vm, mutation):
    if vm['desired']!='running':return []
    if mutation['kind']=='gpu':return [mutation['pci'].lower()] if mutation['pci'] else []
    if mutation['kind']=='start' and mutation['without_gpu']:return []
    if mutation['kind']=='edit':
        selection=mutation['changes']['gpu']
        if selection['kind']=='set_many':return sorted(set(address.lower() for address in selection['pcis']))
        if selection['kind']=='set':return [selection['pci'].lower()] if selection['pci'] else []
    return vm_gpus(vm)


def target_gpu(vm, mutation):
    # Legacy helper for single-device callers; orchestration always uses the set.
    targets=target_gpus(vm,mutation)
    return targets[0] if targets else None


def apply(c, vm, vms, users, mutation):
    uid(vm['id']);uid(vm['owner']);spec=vm['spec']
    assert 1<=spec['memory_gib']<=1024 and 1<=spec['cores']<=256 and 10<=spec['disk_gib']<=65536
    assert 1<=vm['network_slot']<=200 and vm['network_slot'] not in c.get('excluded_network_slots',[]),'conflict: VM bridge overlaps a reviewed uplink subnet'
    assert mutation['kind']!='force_stop' or vm['desired']=='stopped', 'conflict: force stop requires stopped intent'
    if mutation['kind']=='edit' and vm['desired']=='running':
        assert mutation['restart_confirmed'], 'conflict: resource edit requires restart confirmation'
        assert set(target_gpus(vm,mutation)) <= gpu_claims(vm), 'conflict: edit GPUs have not been claimed'
    active=[v for v in vms if v['observed']!='deleted']
    cpu_policy(c,active)
    d=owned(vm,missing=True)
    name=vm_name(vm);slot=vm['network_slot'];bridge=f'hcvm{slot}'
    if vm['desired']=='deleted':
        if d is not None:
            checked_gpu_devices(vm,d)
            stop(vm);api('DELETE','/1.0/instances/'+name)
        managed_host.cleanup(sys.modules[__name__],vm)
        for address in sorted(gpu_claims(vm)):restore_gpu(address)
        restore_released_gpus(c,vms,exclude=vm['id'])
        network=api('GET','/1.0/networks/'+bridge,missing=True)
        if network is not None:
            assert network['config'].get('user.homecloud.id')==vm['id'], 'conflict: foreign network'
            api('DELETE','/1.0/networks/'+bridge)
        release_cpu_policy(c,vm,active)
        instance_runtime.finished(sys.modules[__name__],vm)
        return {'state':'deleted','ssh_host_key':None,'compute_released':True}
    backing(c,users)
    if vm['desired']=='stopped':
        assert d is not None, 'conflict: recorded VM is missing; inspect before releasing compute'
        checked_gpu_devices(vm,d)
        if mutation['kind']=='force_stop':stop(vm,force=True)
        else:stop(vm)
        data={'config':{**d['config'],'boot.autostart':'false'},'devices':{k:v for k,v in d['devices'].items() if k not in gpu_devices(d)},'profiles':[]}
        if mutation['kind'] in ('resize','edit'):
            data['config']['limits.memory']=f'{spec["memory_gib"]}GiB'
            data['devices']['root']['size']=f'{spec["disk_gib"]}GiB'
        api('PUT','/1.0/instances/'+name,data)
        for address in sorted(gpu_claims(vm)):restore_gpu(address)
        release_cpu_policy(c,vm,active)
        return {'state':'stopped','ssh_host_key':vm['ssh_host_key'],'compute_released':True}
    if c.get('minimum_reserves'):memory_capacity.running_guard(c,vms)
    else:assert sum(v['resources']['memory_bytes'] for v in active)<=c['capacity']['memory_bytes'],'conflict: RAM budget exceeded'
    assert vm['resources']['memory_bytes']>=spec['memory_gib']*GIB and vm['resources']['cpu_cores']>=spec['cores'], 'conflict: compute is not claimed'
    network=api('GET','/1.0/networks/'+bridge,missing=True)
    if network is None:
        api('POST','/1.0/networks',{'name':bridge,'type':'bridge','config':{'ipv4.address':f'10.78.{slot}.1/24','ipv4.nat':'true','ipv6.address':'none','dns.mode':'managed','user.homecloud.id':vm['id']}})
    else:assert network['config'].get('user.homecloud.id')==vm['id'], 'conflict: foreign network'
    wanted={'limits.memory':f'{spec["memory_gib"]}GiB','limits.cpu':cpulist(vm['cpu_ids'])}
    if d is None:
        instance_runtime.checkpoint(sys.modules[__name__],vm,'creating')
        assert mutation['kind']=='create','conflict: recorded VM is missing from Incus; do not silently recreate its disk'
        data={'name':name,'type':'virtual-machine','profiles':[],'source':{'type':'image','fingerprint':c['managed_host_image'] if managed_host.enabled(vm) else c['image']},
              'config':{**wanted,'boot.autostart':'false','user.homecloud.id':vm['id'],'user.homecloud.owner':vm['owner'],'raw.apparmor':apparmor(vm),'raw.qemu':f'-name {name},debug-threads=on','cloud-init.user-data':cloud_init(spec['ssh_public_key'], '0.1.2' if 't3' in spec.get('applications', []) else None)},
              'devices':{'root':{'type':'disk','path':'/','pool':storage_pool(vm['owner']),'size':f'{spec["disk_gib"]}GiB'},**seed_devices(c),**managed_host.device(sys.modules[__name__],c,vm,create=True),'eth0':{'type':'nic','network':bridge,'ipv4.address':f'10.78.{slot}.10','security.mac_filtering':'true','security.ipv4_filtering':'true','security.ipv6_filtering':'true'}}}
        api('POST','/1.0/instances',data);d=owned(vm)
    starting=mutation['kind']=='start' or (mutation['kind']=='state' and mutation['desired']=='running')
    sizes_differ=d['config'].get('limits.memory')!=wanted['limits.memory'] or d['devices']['root'].get('size')!=f'{spec["disk_gib"]}GiB'
    pins_differ=d['config'].get('limits.cpu')!=wanted['limits.cpu']
    target=target_gpus(vm,mutation)
    current={dev['pci'].lower() for dev in checked_gpu_devices(vm,d).values()}
    assert set(target) <= gpu_claims(vm), 'conflict: GPUs have not been claimed'
    gpu_preflight(c,vm,target,current)
    if sizes_differ or pins_differ:
        assert mutation['kind'] in ('resize','edit') or (starting and not sizes_differ and state(vm)=='stopped'), 'conflict: instance resource drift; operator review required'
        stop(vm)
        data={'config':{**d['config'],**wanted},'devices':d['devices'],'profiles':[]}
        data['devices']['root']['size']=f'{spec["disk_gib"]}GiB'
        api('PUT','/1.0/instances/'+name,data);d=owned(vm)
    current={dev['pci'].lower() for dev in checked_gpu_devices(vm,d).values()}
    if current!=set(target):
        assert mutation['kind'] in ('gpu','edit') or (starting and state(vm)=='stopped'), 'conflict: GPU assignment drift'
        gpu_changes(c,vm,d,target);d=owned(vm)
    for address in sorted(gpu_claims(vm)-set(target)):restore_gpu(address)
    if d['config'].get('boot.autostart')!='false':
        api('PUT','/1.0/instances/'+name,{'config':{**d['config'],'boot.autostart':'false'},'devices':d['devices'],'profiles':[]})
    start(vm)
    try:status=exec_guest(vm,'cloud-init','status','--format=json',check=False)
    except subprocess.TimeoutExpired:
        instance_runtime.checkpoint(sys.modules[__name__],vm,'guest_agent')
        instance_runtime.wait(sys.modules[__name__],vm,'agent','waiting for the guest provisioning agent',300)
    try:provision=json.loads(status.stdout)
    except ValueError:
        instance_runtime.cid_consistent(sys.modules[__name__],vm)
        instance_runtime.checkpoint(sys.modules[__name__],vm,'guest_agent')
        instance_runtime.wait(sys.modules[__name__],vm,'agent','waiting for the guest provisioning agent',300)
    if provision.get('status')!='done':instance_runtime.checkpoint(sys.modules[__name__],vm,'guest_setup')
    if provision.get('status') in ('error','degraded') or provision.get('errors'):
        raise RuntimeError('conflict: guest setup failed; inspect /var/log/cloud-init-output.log. Fix networking/setup before retrying, or delete this incomplete VM')
    if provision.get('status')!='done':instance_runtime.wait(sys.modules[__name__],vm,'provisioning','waiting for guest provisioning',600)
    if target:instance_runtime.checkpoint(sys.modules[__name__],vm,'gpu_driver')
    for address in target:ensure_gpu_driver(c,{**vm,'gpu':address,'gpus':[address]})
    if len(target)>1:
        probe=exec_guest(vm,'nvidia-smi','--query-gpu=uuid','--format=csv,noheader',check=False)
        assert probe.returncode==0 and len(set(probe.stdout.strip().splitlines()))==len(target), 'conflict: not all selected GPUs are responding to the supported guest query'
    instance_runtime.checkpoint(sys.modules[__name__],vm,'ssh_identity')
    p=exec_guest(vm,'cat','/etc/ssh/ssh_host_ed25519_key.pub',check=False)
    if p.returncode or not p.stdout.startswith('ssh-ed25519 '):raise RuntimeError('pending: waiting for guest SSH host key')
    host_key=p.stdout.strip()
    if vm['ssh_host_key'] and host_key!=vm['ssh_host_key']:raise RuntimeError('conflict: SSH host key changed; explicit operator review required')
    instance_runtime.setup_finished(sys.modules[__name__],vm)
    return {'state':'running','ssh_host_key':host_key}


def pci(address):
    assert re.fullmatch(r'[a-fA-F0-9]{4}:[a-fA-F0-9]{2}:[a-fA-F0-9]{2}\.[0-7]',address)
    return Path('/sys/bus/pci/devices')/address


def gpu_preflight(c,vm,targets,present):
    added=set(targets)-set(present)
    for address in added:
        assert address in c.get('allowed_gpus',[]),'GPU is not offered by the operator'
        gpu_group(c,address)
    if added:
        for other in api('GET','/1.0/instances?recursion=1'):
            if other['name']==vm_name(vm):continue
            assert not any(dev.get('type')=='gpu' and dev.get('pci','').lower() in added for dev in other['expanded_devices'].values()),'conflict: GPU is attached outside this allocation'


def gpu_group(c,address):
    device=pci(address);group=(device/'iommu_group').resolve(strict=True)
    functions=[p for p in (group/'devices').iterdir() if p.name.startswith(address[:-1])]
    peers=[p.name for p in (group/'devices').iterdir() if p.name not in [x.name for x in functions] and not (p/'class').read_text().strip().startswith('0x0604')]
    assert not peers,'Unsafe IOMMU group contains another endpoint'
    display=(device/'boot_vga').exists() and (device/'boot_vga').read_text().strip()=='1'
    assert not display or c.get('allow_host_display_handoff',False),'Host display GPU requires an administrator-selected handoff policy'
    if display:
        drm=device/'drm'
        nodes=[str(Path('/dev/dri')/p.name) for p in drm.iterdir() if re.fullmatch(r'(card|renderD)[0-9]+',p.name)] if drm.exists() else []
        if nodes:
            clients=run('fuser',*nodes,check=False)
            assert not clients.stdout.strip(),'Host GPU has userspace clients'
    return functions,display


def prepare_gpu(c,address):
    functions,display=gpu_group(c,address)
    saved=STATE/('gpu-'+address.replace(':','_')+'.json')
    if not saved.exists():
        record={'drivers':{p.name:(p/'driver').resolve().name if (p/'driver').exists() else None for p in functions},'vtconsoles':[p.name for p in Path('/sys/class/vtconsole').glob('vtcon*') if (p/'bind').read_text().strip()=='1'] if display else [],'getty':run('systemctl','is-active','getty@tty1.service',check=False).returncode==0 if display else False}
        save(saved,record)
    record=json.loads(saved.read_text())
    if display:
        if record['getty']:run('systemctl','stop','getty@tty1.service')
        for name in record['vtconsoles']:(Path('/sys/class/vtconsole')/name/'bind').write_text('0')


def gpu_changes(c,vm,d,targets):
    # Replays preserve existing device names/guest PCI placement for retained GPUs.
    targets=set(targets)
    devices=checked_gpu_devices(vm,d)
    gpu_preflight(c,vm,targets,{dev['pci'].lower() for dev in devices.values()})
    stop(vm)
    removed={name:dev for name,dev in devices.items() if dev['pci'].lower() not in targets}
    if removed:
        api('PUT','/1.0/instances/'+vm_name(vm),{'config':d['config'],'devices':{k:v for k,v in d['devices'].items() if k not in removed},'profiles':[]})
        for address in sorted({dev['pci'].lower() for dev in removed.values()}):restore_gpu(address)
    d=owned(vm)
    devices=checked_gpu_devices(vm,d)
    present={dev['pci'].lower() for dev in devices.values()}
    for address in sorted(targets-present):prepare_gpu(c,address)
    additions=sorted(targets-present)
    if additions:
        updated=dict(d['devices'])
        for address in additions:
            name=next('gpu'+str(n) for n in range(256) if 'gpu'+str(n) not in updated)
            updated[name]={'type':'gpu','gputype':'physical','pci':address}
        api('PUT','/1.0/instances/'+vm_name(vm),{'config':d['config'],'devices':updated,'profiles':[]})


def restore_released_gpus(c,vms,exclude=None):
    assigned={pci for v in vms if v['id']!=exclude and v['observed']!='deleted' for pci in gpu_claims(v)}
    for address in c.get('allowed_gpus',[]):
        if address in assigned:continue
        # Also check foreign Incus instances before restoring a saved handoff.
        attached=any(dev.get('type')=='gpu' and dev.get('pci','').lower()==address.lower() for v in api('GET','/1.0/instances?recursion=1') for dev in v['expanded_devices'].values())
        if not attached:restore_gpu(address)


def ensure_gpu_driver(c,vm):
    probe=exec_guest(vm,'nvidia-smi','--query-gpu=name','--format=csv,noheader',check=False)
    if probe.returncode==0:return
    package=c.get('gpu_driver_packages',{}).get(vm_gpus(vm)[0])
    assert package and re.fullmatch(r'nvidia-driver-[0-9]+',package),'conflict: guest GPU driver is unavailable; configure a supported image or driver package'
    unit='homecloud-gpu-driver.service'
    exists=exec_guest(vm,'systemctl','show',unit,'-p','LoadState','--value',check=False)
    if exists.stdout.strip()=='not-found':
        branch=package.removeprefix('nvidia-driver-')
        # Install Canonical-signed modules first. A direct nvidia-driver install
        # otherwise selects DKMS and requires interactive MOK enrollment.
        script='#!/bin/sh\nset -eu\nexport DEBIAN_FRONTEND=noninteractive\napt-get update\napt-get install -y linux-modules-nvidia-'+branch+'-generic "linux-modules-nvidia-'+branch+'-$(uname -r)"\napt-get install -y '+package+'\n'
        # Incus file push consumes stdin, never interpolates client data into a shell.
        run('incus','file','push','-',vm_name(vm)+'/usr/local/sbin/homecloud-gpu-driver','--mode=0700',input=script)
        exec_guest(vm,'systemd-run','--unit='+unit,'--property=RemainAfterExit=yes','/usr/local/sbin/homecloud-gpu-driver')
        raise RuntimeError('pending: installing the guest GPU driver')
    active=exec_guest(vm,'systemctl','show',unit,'-p','ActiveState','--value',check=False).stdout.strip()
    if active=='failed':raise RuntimeError('conflict: guest GPU driver installation failed; inspect journalctl -u '+unit)
    substate=exec_guest(vm,'systemctl','show',unit,'-p','SubState','--value',check=False).stdout.strip()
    if active!='active' or substate!='exited':raise RuntimeError('pending: installing the guest GPU driver')
    marker=STATE/('gpu-driver-reboot-'+uuid.UUID(vm['id']).hex+'.json')
    if not marker.exists():
        save(marker,{'reboot_requested':True});stop(vm);start(vm)
        raise RuntimeError('pending: rebooted guest to load its GPU driver')
    raise RuntimeError('conflict: guest GPU is not responding after driver installation; operator review required')


def restore_gpu(address):
    saved=STATE/('gpu-'+address.replace(':','_')+'.json')
    if not saved.exists():return
    assert not any(dev.get('type')=='gpu' and dev.get('pci','').lower()==address.lower() for v in api('GET','/1.0/instances?recursion=1') for dev in v['expanded_devices'].values()), 'conflict: GPU is still attached; claim retained'
    record=json.loads(saved.read_text())
    for bdf,original in record['drivers'].items():
        p=pci(bdf);current=(p/'driver').resolve().name if (p/'driver').exists() else None
        if current!=original:
            if current:(p/'driver/unbind').write_text(bdf)
            (p/'driver_override').write_text(original or '\n')
            if original:
                run('modprobe',original)
                Path('/sys/bus/pci/drivers_probe').write_text(bdf)
        actual=(p/'driver').resolve().name if (p/'driver').exists() else None
        assert actual==original, 'conflict: GPU driver handoff incomplete; claim retained'
        (p/'driver_override').write_text('\n')
    for name in record['vtconsoles']:
        try:(Path('/sys/class/vtconsole')/name/'bind').write_text('1')
        except OSError:pass
    if record['getty']:run('systemctl','start','getty@tty1.service')
    saved.unlink()


def main():
    assert os.geteuid()==0,'Run through the installed privileged adapter'
    c=load_config();STATE.mkdir(parents=True,exist_ok=True)
    if len(sys.argv)>1:
        assert sys.argv[1:] == ['cpu-layout']
        with (STATE/'operations.lock').open('a') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX)
            c=load_config()
            resource_reserves.boot(sys.modules[__name__],c,json.loads((STATE/'cpu-vms.json').read_text()) if (STATE/'cpu-vms.json').exists() else [])
            return {'kind':'ok'}
    raw=sys.stdin.read(1024*1024+1)
    assert len(raw)<=1024*1024,'Host request exceeds limit'
    request=json.loads(raw)
    kind=request['kind']
    if kind=='shutdown_status':return {'kind':'shutdown_boot','data':shutdown_boot()}
    if kind=='diagnostics':return {'kind':'diagnostics','data':instance_runtime.diagnostics(sys.modules[__name__],request['vm'])}
    if kind=='application':return {'kind':'application','data':managed_apps.application(sys.modules[__name__],request['vm'],request['application'],request['command'])}
    if kind=='reserve_status':return {'kind':'reserve_policy','data':resource_reserves.report(c['_reserve_base'],c['_reserve_record'])}
    if kind=='storage':return {'kind':'storage','data':json.loads(run('/usr/local/bin/homecloudd','collect-storage',timeout=120).stdout)}
    if kind=='verify_ssh':
        vm=request['vm'];d=owned(vm);assert state(vm)=='running','VM is not running'
        eth=d['devices'].get('eth0',{});slot=vm['network_slot']
        assert eth.get('network')==f'hcvm{slot}' and eth.get('ipv4.address')==f'10.78.{slot}.10','conflict: VM endpoint drift'
        network=api('GET','/1.0/networks/'+f'hcvm{slot}')
        assert network['config'].get('user.homecloud.id')==vm['id'],'conflict: network ownership changed'
        return {'kind':'ok'}
    with (STATE/'operations.lock').open('a') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        c=load_config()
        if kind=='cancel_shutdown':return cancel_shutdown(request['operation_id'],request['boot_id'])
        if kind=='check_shutdown_fence':return check_shutdown_fence(request['operation_id'],request['boot_id'])
        if kind=='check_shutdown':return check_shutdown(request['boot_id'],request['vms'])
        if kind=='drain_guest':return drain_guest(request['operation_id'],request['vm'],request['boot_id'])
        if kind=='power_off':return power_off(request['operation_id'],request['boot_id'],request['vms'])
        if kind=='cancel_memory_capacity':return {'kind':'memory_capacity_cancelled','data':memory_capacity.cancel(sys.modules[__name__],c,request['operation'])}
        if kind=='adopt_memory_capacity':return {'kind':'memory_capacity','data':memory_capacity.adopt(sys.modules[__name__],c,request['operation'])}
        if kind=='discard_reserves':return {'kind':'discarded_reserves','data':resource_reserves.discard(sys.modules[__name__],c,request['operation'])}
        if kind=='apply_reserves':return {'kind':'reserve_policy','data':resource_reserves.apply(sys.modules[__name__],c,request['operation'],request['vms'],request['users'])}
        releasing=kind=='apply' and request['vm']['desired'] in ('stopped','deleted')
        assert releasing or c['_reserve_record']['phase']=='applied','Host reserve update needs recovery before workload changes'
        if kind=='backing':backing(c,request['users']);return {'kind':'ok'}
        if kind=='apply':
            instance_runtime.fence(sys.modules[__name__],request['vm'])
            return {'kind':'applied','data':apply(c,request['vm'],request['vms'],request['users'],request['mutation'])}
    raise ValueError('Unknown host operation')


if __name__=='__main__':
    try:print(json.dumps(main()))
    except Exception as error:
        print(json.dumps({'kind':'error','data':str(error)}))
        if len(sys.argv)>1:sys.exit(1)
