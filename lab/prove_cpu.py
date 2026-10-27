"""Inspect enforced cpusets and real execution under concurrent VM/host load."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

CG=Path('/sys/fs/cgroup')
HOUSE={0,1,4,5,6,7,10,11}
VMS={'hclab-vm01':{2,8},'hclab-vm02':{3,9}}
FILE=Path('/var/lib/home-cloud-lab/cpu-isolation.json')


def run(*args,check=True,timeout=120):
    p=subprocess.run(args,capture_output=True,text=True,timeout=timeout)
    if check and p.returncode:raise RuntimeError(f'{args}: {p.stdout}\n{p.stderr}')
    return p


def cpus(s):
    result=set()
    for part in s.strip().split(','):
        if not part:continue
        ends=list(map(int,part.split('-')))
        result.update(range(ends[0],ends[-1]+1))
    return result


def state(vm):
    return json.loads(run('incus','query',f'/1.0/instances/{vm}/state').stdout)


def ready(vm):
    end=time.monotonic()+150
    while time.monotonic()<end:
        if run('incus','exec',vm,'--','true',check=False,timeout=10).returncode==0:return
        time.sleep(2)
    raise RuntimeError(vm+' agent unavailable')


def snapshot(vm):
    pid=state(vm)['pid'];cg=(Path(f'/proc/{pid}/cgroup').read_text().strip().split('::',1)[1])
    assert cg.startswith('/homecloud.slice/homecloud-vm'),cg
    effective=cpus((CG/cg.lstrip('/')/'cpuset.cpus.effective').read_text())
    assert effective==HOUSE|VMS[vm],(vm,effective)
    threads=[]
    for task in Path(f'/proc/{pid}/task').iterdir():
        try:
            name=(task/'comm').read_text().strip();allowed=set(os.sched_getaffinity(int(task.name)))
            fields=(task/'stat').read_text().rsplit(') ',1)[1].split()
        except (FileNotFoundError,ProcessLookupError):continue
        assert allowed<=effective,(vm,name,allowed)
        if '/KVM' in name:
            assert len(allowed)==1 and allowed<=VMS[vm],(name,allowed)
        elif name!='kvm-nx-lpage-re':
            assert allowed<=HOUSE,(vm,'helper outside housekeeping',name,allowed)
        threads.append({'tid':int(task.name),'name':name,'allowed':sorted(allowed),
                        'last_cpu':int(fields[36]),'ticks':int(fields[11])+int(fields[12])})
    vcpus=[t for t in threads if '/KVM' in t['name']]
    assert len(vcpus)==2 and {t['allowed'][0] for t in vcpus}==VMS[vm],vcpus
    return {'pid':pid,'cgroup':cg,'effective_cpus':sorted(effective),'threads':threads}


def interrupts():
    result={}
    for line in Path('/proc/interrupts').read_text().splitlines()[1:]:
        label,_,rest=line.partition(':')
        if not label.strip().isdigit():continue
        fields=rest.split()
        if len(fields)<12:continue
        result[label.strip()]={'counts':list(map(int,fields[:12])),'description':' '.join(fields[12:])}
    return result


LOAD=r'''
import hashlib,json,multiprocessing,os,time
def cpu():
 end=time.monotonic()+25;n=0
 while time.monotonic()<end:hashlib.sha256(b'x'*65536).digest();n+=1
 return n
def io():
 end=time.monotonic()+25;n=0;p='/var/tmp/home-cloud-cpu-io-proof'
 while time.monotonic()<end:
  with open(p,'wb',buffering=0) as f:f.write(os.urandom(1048576));os.fsync(f.fileno())
  n+=1
 os.unlink(p);return n
with multiprocessing.get_context('fork').Pool(3) as pool:
 jobs=[pool.apply_async(cpu),pool.apply_async(cpu),pool.apply_async(io)]
 print(json.dumps({'cpu_iterations':[jobs[0].get(),jobs[1].get()],'io_rounds':jobs[2].get()}))
'''
HOST_LOAD=LOAD.replace("'/var/tmp/home-cloud-cpu-io-proof'","'/var/lib/home-cloud-lab/host-cpu-io-proof'")


assert os.geteuid()==0
report={'passed':False,'housekeeping_cpus':sorted(HOUSE),'dedicated':{k:sorted(v) for k,v in VMS.items()},
        'definition':'VM vCPUs exclusively own full SMT sibling pairs; helpers and host userspace use housekeeping; residual kernel/hardware activity is measured, not a real-time guarantee',
        'cycles':[]}
post_reboot='--post-reboot' in sys.argv
if post_reboot:
    report=json.loads(FILE.read_text());report['passed']=False
    report['post_reboot_boot_id']=Path('/proc/sys/kernel/random/boot_id').read_text().strip()
for vm in VMS:ready(vm)
for unit in ('system.slice','user.slice'):
    assert cpus((CG/unit/'cpuset.cpus.effective').read_text())==HOUSE
assert set(os.sched_getaffinity(1))<=HOUSE,'PID 1 not on housekeeping CPUs'
# An otherwise privileged process placed in VM A's cgroup cannot use B's
# reserved cores through sched_setaffinity. The cgroup is the hard bound.
for vm,other in [('hclab-vm01',3),('hclab-vm02',2)]:
    group=snapshot(vm)['cgroup']
    code='import os;from pathlib import Path;Path('+repr(str(CG/group.lstrip('/')/'cgroup.procs'))+').write_text(str(os.getpid()));os.sched_setaffinity(0,{'+str(other)+'})'
    p=run('python3','-c',code,check=False)
    assert p.returncode!=0 and 'Invalid argument' in p.stderr,p.stderr
report['cross_vm_affinity_escape_rejected']=True
unknown=run('/usr/local/libexec/home-cloud-lab/qemu/qemu-system-x86_64','-S','-name','unallocated-vm','--version',check=False)
assert unknown.returncode==125 and 'No CPU allocation' in unknown.stderr
report['unallocated_vm_launch_rejected']=True

for action in (('host_reboot',) if post_reboot else ('initial','restart','stop_start','incus_daemon_restart')):
    before_pids={vm:state(vm)['pid'] for vm in VMS}
    if action=='restart':
        for vm in VMS:run('incus','restart',vm,'--timeout','30',timeout=180)
    elif action=='stop_start':
        for vm in VMS:run('incus','stop',vm,'--timeout','30',timeout=180)
        for unit in ('system.slice','user.slice'):
            assert cpus((CG/unit/'cpuset.cpus.effective').read_text())==HOUSE
        for vm in VMS:run('incus','start',vm)
    elif action=='incus_daemon_restart':run('systemctl','restart','incus.service')
    for vm in VMS:ready(vm)
    start={vm:snapshot(vm) for vm in VMS}
    if action in ('restart','stop_start'):assert all(start[vm]['pid']!=before_pids[vm] for vm in VMS)
    if action=='incus_daemon_restart':assert all(start[vm]['pid']==before_pids[vm] for vm in VMS)
    irqs_before=interrupts()
    jobs={vm:subprocess.Popen(['incus','exec',vm,'--','python3','-c',LOAD],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True) for vm in VMS}
    jobs['host']=subprocess.Popen(['python3','-c',HOST_LOAD],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    samples=[]
    try:
        for _ in range(12):
            time.sleep(1)
            samples.append({vm:snapshot(vm) for vm in VMS})
            assert set(os.sched_getaffinity(jobs['host'].pid))<=HOUSE
        loads={}
        for label,job in jobs.items():
            out,err=job.communicate(timeout=90)
            assert job.returncode==0,(label,out,err)
            loads[label]=json.loads(out)
            assert all(x>0 for x in loads[label]['cpu_iterations']) and loads[label]['io_rounds']>0
    finally:
        for job in jobs.values():
            if job.poll() is None:job.terminate()
    end={vm:snapshot(vm) for vm in VMS}
    cpu_ticks={}
    for vm in VMS:
        old={t['tid']:t['ticks'] for t in start[vm]['threads'] if '/KVM' in t['name']}
        cpu_ticks[vm]={t['name']:t['ticks']-old[t['tid']] for t in end[vm]['threads'] if '/KVM' in t['name']}
        assert all(t>100 for t in cpu_ticks[vm].values()),cpu_ticks
    irqs_after=interrupts();irq_deltas={}
    for irq,values in irqs_after.items():
        if irq not in irqs_before:continue
        delta=[b-a for a,b in zip(irqs_before[irq]['counts'],values['counts'])]
        reserved_delta=sum(delta[c] for c in {2,3,8,9})
        if reserved_delta:irq_deltas[irq]={'reserved_core_interrupts':reserved_delta,'description':values['description']}
    cycle={'action':action,'start':start,'end':end,'sample_count':len(samples),
           'loads':loads,'vcpu_tick_deltas':cpu_ticks,'hardware_irqs_on_reserved_cores':irq_deltas}
    report['cycles'].append(cycle)
    FILE.write_text(json.dumps(report,indent=2)+'\n')
    print(action+': CPU placement and concurrent load passed',flush=True)
launches=[json.loads(s) for s in Path('/var/log/home-cloud-lab-cpu-launch.jsonl').read_text().splitlines()]
assert len([x for x in launches if x['vm'] in VMS and x['before_qemu_exec']])>=6
report['launch_audit']=launches
report['host_layout']=json.loads(Path('/var/lib/home-cloud-lab/cpu-host-layout.json').read_text())
report['passed']=True
if post_reboot:report['host_reboot_verified']=True
FILE.write_text(json.dumps(report,indent=2)+'\n')
run('incus','stop','hclab-vm02','--timeout','30',timeout=180)
print('Dedicated workload CPU proof passed',flush=True)
