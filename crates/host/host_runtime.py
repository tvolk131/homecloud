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
    p = run(*args, check=not missing)
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
    groups = c['cpu_groups']; house = c['housekeeping']
    assert groups and house and all(g for g in groups)
    cpus = house + [x for g in groups for x in g] + [x for v in c.get('legacy_vms', []) for x in v['cpu_ids']]
    assert len(cpus) == len(set(cpus)) and all(isinstance(x, int) and 0 <= x < 1024 for x in cpus)
    for group in groups:
        siblings = Path(f'/sys/devices/system/cpu/cpu{group[0]}/topology/thread_siblings_list').read_text().strip()
        assert parse_cpus(siblings) == set(group), 'CPU allocation splits a physical core'
    return c


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


def stop(vm):
    if state(vm) == 'stopped': return
    p = run('incus', 'stop', vm_name(vm), '--timeout', '30', check=False, timeout=60)
    if p.returncode and state(vm) != 'stopped':
        raise RuntimeError('conflict: guest did not stop gracefully; local operator review required')


def start(vm):
    if state(vm) == 'running': return
    p = run('incus', 'start', vm_name(vm), check=False, timeout=120)
    if p.returncode and state(vm) != 'running': raise RuntimeError(p.stderr)


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


def cpu_policy(c, vms):
    house = c['housekeeping']; entries = list(c.get('legacy_vms', []))
    for vm in vms:
        if vm['observed'] == 'deleted': continue
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
        run('systemctl','start',entry['unit'])
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
            'packages':['openssh-server','curl','git','ca-certificates'],
            'write_files':[{'path':'/etc/ssh/sshd_config','owner':'root:root','permissions':'0644','content':ssh}],
            'runcmd':[['install','-d','-m','0755','/run/sshd'],['sshd','-t'],['systemctl','enable','--now','ssh'],['systemctl','restart','ssh'],['loginctl','enable-linger','dev']]}
    if t3_version:
        assert re.fullmatch(r'[0-9A-Za-z.+-]{1,80}',t3_version),'Invalid pinned T3 version'
        script=Path(__file__).with_name('setup-t3.sh').read_text()
        config['write_files'].append({'path':'/usr/local/share/homecloud/setup-t3.sh','owner':'root:root','permissions':'0644','content':script})
        config['runcmd'].append(['runuser','-l','dev','-c','sh /usr/local/share/homecloud/setup-t3.sh '+t3_version])
    return '#cloud-config\n'+json.dumps(config)


def apply(c, vm, vms, users, mutation):
    uid(vm['id']);uid(vm['owner']);spec=vm['spec']
    assert 1<=spec['memory_gib']<=1024 and 1<=spec['cores']<=256 and 10<=spec['disk_gib']<=65536
    assert 1<=vm['network_slot']<=200
    active=[v for v in vms if v['observed']!='deleted']
    assert sum(v['resources']['memory_bytes'] for v in active)<=c['capacity']['memory_bytes'], 'conflict: RAM budget exceeded'
    cpu_policy(c,active)
    d=owned(vm,missing=True)
    name=vm_name(vm);slot=vm['network_slot'];bridge=f'hcvm{slot}'
    if vm['desired']=='deleted':
        old_gpu=d['devices'].get('gpu0',{}).get('pci') if d else None
        if d is not None:
            stop(vm);api('DELETE','/1.0/instances/'+name)
        if old_gpu:restore_gpu(old_gpu)
        restore_released_gpus(c,vms,exclude=vm['id'])
        network=api('GET','/1.0/networks/'+bridge,missing=True)
        if network is not None:
            assert network['config'].get('user.homecloud.id')==vm['id'], 'conflict: foreign network'
            api('DELETE','/1.0/networks/'+bridge)
        cpu_policy(c,[v for v in active if v['id']!=vm['id']])
        unit='homecloud-'+uuid.UUID(vm['id']).hex+'.service'
        run('systemctl','stop',unit,check=False)
        return {'state':'deleted','ssh_host_key':None}
    backing(c,users)
    network=api('GET','/1.0/networks/'+bridge,missing=True)
    if network is None:
        api('POST','/1.0/networks',{'name':bridge,'type':'bridge','config':{'ipv4.address':f'10.78.{slot}.1/24','ipv4.nat':'true','ipv6.address':'none','dns.mode':'managed','user.homecloud.id':vm['id']}})
    else: assert network['config'].get('user.homecloud.id')==vm['id'], 'conflict: foreign network'
    wanted={'limits.memory':f'{spec["memory_gib"]}GiB','limits.cpu':cpulist(vm['cpu_ids'])}
    if d is None:
        assert mutation['kind']=='create','conflict: recorded VM is missing from Incus; do not silently recreate its disk'
        data={'name':name,'type':'virtual-machine','profiles':[],'source':{'type':'image','fingerprint':c['image']},
              'config':{**wanted,'boot.autostart':'true' if vm['desired']=='running' else 'false','user.homecloud.id':vm['id'],'user.homecloud.owner':vm['owner'],'raw.apparmor':apparmor(vm),'raw.qemu':f'-name {name},debug-threads=on','cloud-init.user-data':cloud_init(spec['ssh_public_key'],c.get('t3_version'))},
              'devices':{'root':{'type':'disk','path':'/','pool':storage_pool(vm['owner']),'size':f'{spec["disk_gib"]}GiB'},'cloud-init':{'type':'disk','source':'cloud-init:config'},'eth0':{'type':'nic','network':bridge,'ipv4.address':f'10.78.{slot}.10','security.mac_filtering':'true','security.ipv4_filtering':'true','security.ipv6_filtering':'true'}}}
        api('POST','/1.0/instances',data)
        d=owned(vm)
    mismatched=any(d['config'].get(k)!=v for k,v in wanted.items()) or d['devices']['root'].get('size')!=f'{spec["disk_gib"]}GiB'
    if mismatched:
        assert mutation['kind']=='resize', 'conflict: instance resource drift; operator review required'
        stop(vm)
        data={'config':{**d['config'],**wanted},'devices':d['devices'],'profiles':[]}
        data['devices']['root']['size']=f'{spec["disk_gib"]}GiB'
        api('PUT','/1.0/instances/'+name,data);d=owned(vm)
    current_gpu=d['devices'].get('gpu0',{}).get('pci')
    if current_gpu is None and vm['gpu'] is None:restore_released_gpus(c,vms)
    if current_gpu!=vm['gpu']:
        assert mutation['kind']=='gpu', 'conflict: GPU assignment drift'
        gpu_change(c,vm,d)
        d=owned(vm)
    desired_boot='true' if vm['desired']=='running' else 'false'
    if d['config'].get('boot.autostart')!=desired_boot:
        api('PUT','/1.0/instances/'+name,{'config':{**d['config'],'boot.autostart':desired_boot},'devices':d['devices'],'profiles':[]})
    if vm['desired']=='stopped':
        stop(vm);return {'state':'stopped','ssh_host_key':vm['ssh_host_key']}
    start(vm)
    status=exec_guest(vm,'cloud-init','status','--format=json',check=False)
    try:provision=json.loads(status.stdout)
    except ValueError:raise RuntimeError('pending: waiting for guest provisioning agent')
    if provision.get('status') in ('error','degraded') or provision.get('errors'):
        raise RuntimeError('conflict: cloud-init failed; inspect the guest provisioning log')
    if provision.get('status')!='done':raise RuntimeError('pending: waiting for guest provisioning')
    if vm['gpu']:ensure_gpu_driver(c,vm)
    p=exec_guest(vm,'cat','/etc/ssh/ssh_host_ed25519_key.pub',check=False)
    if p.returncode or not p.stdout.startswith('ssh-ed25519 '):raise RuntimeError('pending: waiting for guest SSH host key')
    host_key=p.stdout.strip()
    if vm['ssh_host_key'] and host_key!=vm['ssh_host_key']:raise RuntimeError('conflict: SSH host key changed; explicit operator review required')
    return {'state':'running','ssh_host_key':host_key}


def pci(address):
    assert re.fullmatch(r'[a-fA-F0-9]{4}:[a-fA-F0-9]{2}:[a-fA-F0-9]{2}\.[0-7]',address)
    return Path('/sys/bus/pci/devices')/address


def gpu_change(c,vm,d):
    # Kept as a separate narrow operation; state is saved before host-driver
    # handoff and reused on retries. No idle-based automatic reclamation.
    stop(vm)
    old=d['devices'].get('gpu0',{}).get('pci');new=vm['gpu']
    if old:
        data={'config':d['config'],'devices':{k:v for k,v in d['devices'].items() if k!='gpu0'},'profiles':[]}
        api('PUT','/1.0/instances/'+vm_name(vm),data)
        restore_gpu(old)
    if new:
        assert new in c.get('allowed_gpus',[]),'GPU is not offered by the operator'
        for other in api('GET','/1.0/instances?recursion=1'):
            if other['name']==vm_name(vm):continue
            assert not any(dev.get('type')=='gpu' and dev.get('pci')==new for dev in other['expanded_devices'].values()),'conflict: GPU is attached outside this allocation'
        device=pci(new);group=(device/'iommu_group').resolve(strict=True)
        functions=[p for p in (group/'devices').iterdir() if p.name.startswith(new[:-1])]
        peers=[p.name for p in (group/'devices').iterdir() if p.name not in [x.name for x in functions] and not (p/'class').read_text().strip().startswith('0x0604')]
        assert not peers,'Unsafe IOMMU group contains another endpoint'
        host_display=(device/'boot_vga').exists() and (device/'boot_vga').read_text().strip()=='1'
        assert not host_display or c.get('allow_host_display_handoff',False),'Host display GPU requires an administrator-selected handoff policy'
        saved=STATE/('gpu-'+new.replace(':','_')+'.json')
        if not saved.exists():
            record={'drivers':{p.name:(p/'driver').resolve().name if (p/'driver').exists() else None for p in functions},'vtconsoles':[p.name for p in Path('/sys/class/vtconsole').glob('vtcon*') if (p/'bind').read_text().strip()=='1'] if host_display else [],'getty':run('systemctl','is-active','getty@tty1.service',check=False).returncode==0 if host_display else False}
            save(saved,record)
        record=json.loads(saved.read_text())
        if host_display:
            clients=run('fuser','/dev/dri/card0','/dev/dri/renderD128',check=False)
            assert not clients.stdout.strip(),'Host GPU has userspace clients'
            if record['getty']:run('systemctl','stop','getty@tty1.service')
            for name in record['vtconsoles']:(Path('/sys/class/vtconsole')/name/'bind').write_text('0')
        d=owned(vm);data={'config':d['config'],'devices':{**d['devices'],'gpu0':{'type':'gpu','gputype':'physical','pci':new}},'profiles':[]}
        api('PUT','/1.0/instances/'+vm_name(vm),data)


def restore_released_gpus(c,vms,exclude=None):
    assigned={v['gpu'] for v in vms if v['id']!=exclude and v['gpu'] and v['observed']!='deleted'}
    for address in c.get('allowed_gpus',[]):
        if address in assigned:continue
        # Also check foreign Incus instances before restoring a saved handoff.
        attached=any(dev.get('type')=='gpu' and dev.get('pci')==address for v in api('GET','/1.0/instances?recursion=1') for dev in v['expanded_devices'].values())
        if not attached:restore_gpu(address)


def ensure_gpu_driver(c,vm):
    probe=exec_guest(vm,'nvidia-smi','--query-gpu=name','--format=csv,noheader',check=False)
    if probe.returncode==0:return
    package=c.get('gpu_driver_packages',{}).get(vm['gpu'])
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
    record=json.loads(saved.read_text())
    for bdf,original in record['drivers'].items():
        p=pci(bdf);current=(p/'driver').resolve().name if (p/'driver').exists() else None
        if current!=original:
            if current:(p/'driver/unbind').write_text(bdf)
            (p/'driver_override').write_text(original or '\n')
            if original:run('modprobe',original)
            Path('/sys/bus/pci/drivers_probe').write_text(bdf)
        (p/'driver_override').write_text('\n')
    for name in record['vtconsoles']:
        try:(Path('/sys/class/vtconsole')/name/'bind').write_text('1')
        except OSError:pass
    if record['getty']:run('systemctl','start','getty@tty1.service')
    saved.unlink()


def inventory(c):
    cpu=Path('/proc/cpuinfo').read_text();mem=Path('/proc/meminfo').read_text()
    groups=set()
    for p in Path('/sys/devices/system/cpu').glob('cpu[0-9]*/topology/thread_siblings_list'):groups.add(p.read_text().strip())
    dmar=Path('/sys/firmware/acpi/tables/DMAR').exists();ivrs=Path('/sys/firmware/acpi/tables/IVRS').exists()
    count=len(list(Path('/sys/kernel/iommu_groups').iterdir()))
    ready=count>0
    board=Path('/sys/class/dmi/id/board_name').read_text().strip()
    iommu={'state':'ready' if ready else 'needs_firmware_configuration','evidence':[f'DMAR={dmar}; IVRS={ivrs}; IOMMU groups={count}',f'Motherboard: {board}'],'next_steps':[] if ready else ['Enable Intel VT-d or AMD IOMMU in firmware, then reboot and recheck.','If already enabled, inspect kernel IOMMU configuration. Missing groups alone do not prove unsupported hardware.']}
    cards=[];instances=api('GET','/1.0/instances?recursion=1')
    for p in Path('/sys/bus/pci/devices').iterdir():
        if not (p/'class').read_text().strip().startswith(('0x0300','0x0302')):continue
        group=int((p/'iommu_group').resolve().name) if (p/'iommu_group').exists() else None
        display=(p/'boot_vga').exists() and (p/'boot_vga').read_text().strip()=='1'
        offered=p.name in c.get('allowed_gpus',[])
        state='ready' if ready and offered else ('reserved_by_operator' if not offered else 'needs_firmware_configuration')
        if display and not c.get('allow_host_display_handoff',False):state='host_display_requires_policy'
        members=[]
        if group is not None:
            members=list(((p/'iommu_group').resolve()/'devices').iterdir())
            if any(not member.name.startswith(p.name[:-1]) and not (member/'class').read_text().strip().startswith('0x0604') for member in members):state='unsafe_iommu_group'
        else:state='needs_firmware_configuration'
        owner=next((v for v in instances if any(dev.get('type')=='gpu' and dev.get('pci')==p.name for dev in v['expanded_devices'].values())),None)
        assigned=None;telemetry='Unavailable: GPU is on the host or not responding to a supported guest query.'
        if owner:
            state='assigned'
            try:assigned=uid(owner['config'].get('user.homecloud.id',''))
            except (ValueError,AttributeError):pass
            telemetry='Unavailable while the assigned VM is stopped or its guest driver is unavailable.'
            if owner['status']=='Running':
                try:
                    probe=run('incus','exec',owner['name'],'--','nvidia-smi','--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu','--format=csv,noheader,nounits',check=False,timeout=4)
                    if probe.returncode==0:
                        values=probe.stdout.strip().split(',')
                        if len(values)==4:telemetry=f'Guest GPU responding: {values[0].strip()}% utilization; {values[1].strip()}/{values[2].strip()} MiB VRAM; {values[3].strip()} C. Guest-reported, not an independent hardware health guarantee.'
                except subprocess.TimeoutExpired:pass
        cards.append({'pci':p.name,'model':run('lspci','-s',p.name,check=False).stdout.strip(),'driver':(p/'driver').resolve().name if (p/'driver').exists() else None,'iommu_group':group,'host_display':display,'assigned_vm':assigned,'telemetry':telemetry,'readiness':{'state':state,'evidence':[f'IOMMU group {group}',f'Members: {", ".join(m.name for m in members)}',f'Host display: {display}'],'next_steps':[] if state in ('ready','assigned') else ['Review firmware, group membership, and operator assignment policy.']}})
    available=zget(c['pool'],'available');warnings=[]
    if available<c['minimum_pool_available_bytes']:warnings.append('Pool accounting headroom below operating floor; growth is blocked')
    return {'hostname':os.uname().nodename,'cpu_model':next((l.split(':',1)[1].strip() for l in cpu.splitlines() if l.startswith('model name')),'unknown'),'physical_cores':len(groups),'memory_bytes':int(next(l.split()[1] for l in mem.splitlines() if l.startswith('MemTotal:')))*1024,'pool':c['pool'],'pool_available_bytes':available,'pool_health':run('zpool','list','-H','-o','health',c['pool']).stdout.strip(),'iommu':iommu,'gpus':cards,'warnings':warnings}


def observe(vms):
    actual={v['name']:v for v in api('GET','/1.0/instances?recursion=1')};result=[]
    for vm in vms:
        if vm['observed']=='deleted':continue
        record=actual.get(vm_name(vm));error=None;observed='missing'
        if record:
            observed=record['status'].lower();conf=record['config'];devices=record['devices']
            if conf.get('user.homecloud.id')!=vm['id'] or conf.get('user.homecloud.owner')!=vm['owner']:
                error='Ownership drift: operator review required'
            elif conf.get('limits.cpu')!=cpulist(vm['cpu_ids']) or conf.get('limits.memory')!=f'{vm["spec"]["memory_gib"]}GiB' or devices.get('root',{}).get('size')!=f'{vm["spec"]["disk_gib"]}GiB' or devices.get('gpu0',{}).get('pci')!=vm['gpu']:
                error='Configuration drift: operator review required'
            elif observed!=vm['desired']:
                error='Lifecycle differs from requested state; inspect before restarting'
        else:error='Recorded VM is missing from Incus; resources remain held for inspection'
        result.append({'id':vm['id'],'state':observed,'error':error})
    return result


def main():
    assert os.geteuid()==0,'Run through the installed privileged adapter'
    c=load_config();STATE.mkdir(parents=True,exist_ok=True)
    if len(sys.argv)>1:
        assert sys.argv[1:] == ['cpu-layout']
        cpu_layout(c)
        cpu_policy(c,json.loads((STATE/'cpu-vms.json').read_text()) if (STATE/'cpu-vms.json').exists() else [])
        return {'kind':'ok'}
    raw=sys.stdin.read(1024*1024+1)
    assert len(raw)<=1024*1024,'Host request exceeds limit'
    request=json.loads(raw)
    kind=request['kind']
    if kind=='inventory':return {'kind':'inventory','data':inventory(c)}
    if kind=='observe':return {'kind':'observations','data':observe(request['vms'])}
    if kind=='verify_ssh':
        vm=request['vm'];d=owned(vm);assert state(vm)=='running','VM is not running'
        eth=d['devices'].get('eth0',{});slot=vm['network_slot']
        assert eth.get('network')==f'hcvm{slot}' and eth.get('ipv4.address')==f'10.78.{slot}.10','conflict: VM endpoint drift'
        network=api('GET','/1.0/networks/'+f'hcvm{slot}')
        assert network['config'].get('user.homecloud.id')==vm['id'],'conflict: network ownership changed'
        return {'kind':'ok'}
    lock=(STATE/'operations.lock').open('a');fcntl.flock(lock,fcntl.LOCK_EX)
    if kind=='backing':backing(c,request['users']);return {'kind':'ok'}
    if kind=='apply':return {'kind':'applied','data':apply(c,request['vm'],request['vms'],request['users'],request['mutation'])}
    raise ValueError('Unknown host operation')


if __name__=='__main__':
    try:print(json.dumps(main()))
    except Exception as error:
        print(json.dumps({'kind':'error','data':str(error)}))
        if len(sys.argv)>1:sys.exit(1)
