"""Install/restore the test host's static per-VM CPU layout.

Prototype only: a future controller must derive this policy transactionally
from its resource ledger. No user-configurable raw Incus settings are exposed.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT=Path('/usr/local/libexec/home-cloud-lab')
STATE=Path('/var/lib/home-cloud-lab/cpu-before.json')
HOUSE='0,1,4,5,6,7,10,11'
VMS={'hclab-vm01':('2,8','0,1,2,4,5,6,7,8,10,11','homecloud-vm01.service'),
     'hclab-vm02':('3,9','0,1,3,4,5,6,7,9,10,11','homecloud-vm02.service')}
FILES=[Path('/etc/systemd/system/homecloud.slice'),
       Path('/etc/systemd/system/home-cloud-lab-cpu-layout.service'),
       Path('/etc/systemd/system/incus.service.d/home-cloud-lab-cpu.conf'),
       Path('/etc/systemd/system/system.slice.d/home-cloud-lab-cpu.conf'),
       Path('/etc/systemd/system/user.slice.d/home-cloud-lab-cpu.conf')]
FILES += [Path('/etc/systemd/system')/v[2] for v in VMS.values()]
FILES += [Path('/etc/home-cloud-lab/incus-cpu.env')]


def run(*args,check=True,timeout=120):
    p=subprocess.run(args,capture_output=True,text=True,timeout=timeout)
    if check and p.returncode:raise RuntimeError(f'{args}: {p.stdout}\n{p.stderr}')
    return p


def layout():
    house={int(x) for x in HOUSE.split(',')}; mask=f'{sum(1<<x for x in house):x}'
    state={'housekeeping_cpus':HOUSE,'irqs':{},'workqueues':{},'kernel_threads_moved':0,'kernel_threads_unmovable':[]}
    os.sched_setaffinity(1,house)
    state['pid1_affinity']=sorted(os.sched_getaffinity(1))
    Path('/proc/irq/default_smp_affinity').write_text(mask)
    for p in Path('/proc/irq').glob('*/smp_affinity_list'):
        try:p.write_text(HOUSE)
        except OSError as e:state['irqs'][p.parent.name]={'write_error':str(e)}
        effective=p.with_name('effective_affinity_list')
        if effective.exists():state['irqs'].setdefault(p.parent.name,{})['effective']=effective.read_text().strip()
    for p in Path('/sys/devices/virtual/workqueue').glob('**/cpumask'):
        try:p.write_text(mask);state['workqueues'][str(p)]={'mask':p.read_text().strip()}
        except OSError as e:state['workqueues'][str(p)]={'error':str(e)}
    for p in Path('/proc').iterdir():
        if not p.name.isdigit():continue
        try:
            if (p/'cmdline').read_bytes():continue
            current=os.sched_getaffinity(int(p.name))
            if len(current)>1:
                os.sched_setaffinity(int(p.name),house);state['kernel_threads_moved']+=1
        except (OSError,ValueError) as e:
            if p.exists():state['kernel_threads_unmovable'].append({'pid':p.name,'error':str(e)})
    Path('/var/lib/home-cloud-lab/cpu-host-layout.json').write_text(json.dumps(state,indent=2)+'\n')
    print(json.dumps(state,indent=2))


def stop_guests():
    for vm in VMS:run('incus','stop',vm,'--timeout','30',check=False,timeout=180)


assert os.geteuid()==0
mode=sys.argv[1]
if mode=='layout':
    layout();raise SystemExit(0)
if mode=='restore':
    before=json.loads(STATE.read_text());stop_guests()
    for vm,props in before['vms'].items():
        for k,v in props.items():
            if v is None:run('incus','config','unset',vm,k)
            else:run('incus','config','set',vm,f'{k}={v}')
    for p in FILES:p.unlink(missing_ok=True)
    run('systemctl','daemon-reload')
    for unit in ('system.slice','user.slice'):
        run('systemctl','set-property','--runtime',unit,'AllowedCPUs='+before['slice_cpus'][unit])
    for _,_,unit in VMS.values():run('systemctl','stop',unit,check=False)
    for path,value in before['kernel_files'].items():
        try:Path(path).write_text(value)
        except OSError:pass
    os.sched_setaffinity(1,set(before['pid1_affinity']))
    run('systemctl','restart','incus.service')
    for vm in before['running']:run('incus','start',vm,check=False)
    print('Restored original CPU configuration');raise SystemExit(0)
assert mode=='install'
assert not STATE.exists(),'CPU installation already exists; inspect before reapplying'
assert not any(p.exists() for p in FILES)
assert Path('/sys/devices/system/cpu/cpu2/topology/thread_siblings_list').read_text().strip()=='2,8'
assert Path('/sys/devices/system/cpu/cpu3/topology/thread_siblings_list').read_text().strip()=='3,9'
instances=json.loads(run('incus','list','--format','json').stdout)
assert {x['name'] for x in instances}==set(VMS),'Unknown VM: CPU policy must account for all workloads'
before={'pid1_affinity':sorted(os.sched_getaffinity(1)),'running':[x['name'] for x in instances if x['status']=='Running'],'vms':{},'kernel_files':{},'slice_cpus':{}}
for vm in VMS:
    d=json.loads(run('incus','query',f'/1.0/instances/{vm}').stdout)
    assert d['expanded_config'].get('user.home-cloud-lab')=='true'
    before['vms'][vm]={k:d['config'].get(k) for k in ('raw.apparmor','raw.qemu')}
for unit in ('system.slice','user.slice'):
    before['slice_cpus'][unit]=Path('/sys/fs/cgroup',unit,'cpuset.cpus').read_text().strip()
paths=list(Path('/proc/irq').glob('*/smp_affinity_list'))+[Path('/proc/irq/default_smp_affinity')]
paths+=list(Path('/sys/devices/virtual/workqueue').glob('**/cpumask'))
for p in paths:
    try:before['kernel_files'][str(p)]=p.read_text()
    except OSError:pass
# Install build tools only on this disposable proving host.
run('apt-get','install','-y','--no-install-recommends','gcc','libc6-dev',timeout=600)
bindir=ROOT/'qemu';bindir.mkdir(exist_ok=True)
run('cc','-O2','-Wall','-Wextra','-Werror','-o',str(bindir/'qemu-system-x86_64'),str(ROOT/'qemu_cpu_launcher.c'))
STATE.write_text(json.dumps(before,indent=2)+'\n')
stop_guests()
def write(path,text):
    path.parent.mkdir(parents=True,exist_ok=True);path.write_text(text)
write(FILES[0],'[Unit]\nDescription=Home Cloud VM CPU domains\n[Slice]\nCPUAccounting=yes\n')
for unit in ('system.slice','user.slice'):
    write(Path('/etc/systemd/system')/(unit+'.d')/'home-cloud-lab-cpu.conf',f'[Slice]\nAllowedCPUs={HOUSE}\n')
policy=[]
for vm,(dedicated,allowed,unit) in VMS.items():
    group=f'/sys/fs/cgroup/homecloud.slice/{unit}'
    write(Path('/etc/systemd/system')/unit,f'''[Unit]
Description=Home Cloud CPU domain for {vm}
Before=incus.service
[Service]
Type=simple
ExecStart=/usr/bin/sleep infinity
Delegate=yes
Slice=homecloud.slice
AllowedCPUs={allowed}
CPUAffinity={HOUSE.replace(',', ' ')}
''')
    policy.append(f'{vm}\t{allowed}\t{HOUSE}\t{group}\n')
    old=before['vms'][vm]['raw.apparmor'] or ''
    rules=f'''\n# Home Cloud CPU launcher (lab prototype)
/usr/bin/qemu-system-x86_64 rix,
/etc/home-cloud-lab/cpu-policy.tsv r,
{group}/cgroup.procs w,
{group}/cpuset.cpus.effective r,
/var/log/home-cloud-lab-cpu-launch.jsonl w,
'''
    run('incus','config','set',vm,'raw.apparmor='+old+rules)
    # Name vCPU threads for reliable runtime assertions; launch policy uses
    # the immutable controller-selected Incus instance name.
    run('incus','config','set',vm,f'raw.qemu=-name {vm},debug-threads=on')
Path('/etc/home-cloud-lab/cpu-policy.tsv').write_text(''.join(policy))
Path('/etc/home-cloud-lab/cpu-policy.tsv').chmod(0o644)
Path('/var/log/home-cloud-lab-cpu-launch.jsonl').touch(mode=0o644)
write(FILES[1],f'''[Unit]
Description=Home Cloud CPU housekeeping and IRQ policy
Before=incus.service
[Service]
Type=oneshot
ExecStart=/usr/bin/python3 {ROOT}/configure_cpu.py layout
RemainAfterExit=yes
''')
write(Path('/etc/home-cloud-lab/incus-cpu.env'),f'PATH={bindir}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n')
units=' '.join(v[2] for v in VMS.values())
write(FILES[2],f'''[Unit]
Requires=home-cloud-lab-cpu-layout.service {units}
After=home-cloud-lab-cpu-layout.service {units}
[Service]
EnvironmentFile=/etc/home-cloud-lab/incus-cpu.env
CPUAffinity={HOUSE.replace(',', ' ')}
''')
run('systemctl','daemon-reload')
for _,_,unit in VMS.values():run('systemctl','start',unit)
for unit in ('system.slice','user.slice'):run('systemctl','set-property','--runtime',unit,'AllowedCPUs='+HOUSE)
run('systemctl','start','home-cloud-lab-cpu-layout.service')
run('systemctl','restart','incus.service')
# boot.autostart can race an explicit start after the daemon returns. Reobserve
# the desired state instead of treating an already-running response as failure.
import time
for vm in VMS:
    deadline=time.monotonic()+120
    while time.monotonic()<deadline:
        state=json.loads(run('incus','query',f'/1.0/instances/{vm}/state').stdout)
        if state['status']=='Running':break
        if state['status']=='Stopped':
            p=run('incus','start',vm,check=False)
            if p.returncode and 'already running' not in p.stderr and 'currently running' not in p.stderr:
                raise RuntimeError(p.stdout+p.stderr)
        time.sleep(2)
    else:raise RuntimeError(vm+' did not start')
print('CPU launch policy installed; both lab VMs started',flush=True)
