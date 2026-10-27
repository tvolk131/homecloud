"""Capture the final lab state and distinguish proved primitives from app work."""
import json
import os
from pathlib import Path
import subprocess
import time

ROOT=Path('/var/lib/home-cloud-lab')


def run(*a):
    return subprocess.check_output(a,text=True).strip()


def evidence(name):
    p=ROOT/(name+'.json')
    return json.loads(p.read_text()) if p.exists() else {}


instances=[]
for vm in json.loads(run('incus','list','--format','json')):
    state=json.loads(run('incus','query',f'/1.0/instances/{vm["name"]}/state'))
    item={'name':vm['name'],'status':vm['status'],
          'memory':vm['expanded_config'].get('limits.memory'),
          'cpu':vm['expanded_config'].get('limits.cpu'),
          'gpu_devices':[k for k,d in vm['expanded_devices'].items() if d['type']=='gpu']}
    if state.get('pid',0)>0:
        cg=Path(f'/proc/{state["pid"]}/cgroup').read_text().strip().split('::',1)[1]
        item['cgroup']=cg
        item['effective_cpus']=Path('/sys/fs/cgroup',cg.lstrip('/'),'cpuset.cpus.effective').read_text().strip()
    instances.append(item)
gpu=evidence('gpu-handoff-state')
result={
    'captured_at':time.time(),'instances':instances,'pool_health':run('zpool','status','-x','hclab'),
    'firewall':run('systemctl','is-active','home-cloud-lab-firewall.service'),
    'cpu_policy_service':run('systemctl','is-active','home-cloud-lab-cpu-layout.service'),
    'host_housekeeping_cpus':Path('/sys/fs/cgroup/system.slice/cpuset.cpus.effective').read_text().strip(),
    'dmar_present':Path('/sys/firmware/acpi/tables/DMAR').exists(),
    'iommu_group_count':len(list(Path('/sys/kernel/iommu_groups').iterdir())),
    'host_framebuffer':Path('/proc/fb').read_text().strip(),
    'host_console_service':run('systemctl','is-active','getty@tty1.service'),
    'gpu_released':gpu.get('released'),
    'restored_pci_drivers':{b:(Path('/sys/bus/pci/devices')/b/'driver').resolve().name for b in ['0000:01:00.0','0000:01:00.1']},
    'recovery_timer_active':subprocess.run(['systemctl','is-active',gpu.get('recovery_timer','missing.timer')],capture_output=True).returncode==0,
    'zfs_arc_max':Path('/sys/module/zfs/parameters/zfs_arc_max').read_text().strip(),
    'host_reboot_tested':evidence('cpu-isolation').get('host_reboot_verified',False),
    'multiuser_storage_exhaustion_tested':evidence('storage-guarantees').get('passed',False),
    'dedicated_cpu_workload_isolation_tested':evidence('cpu-isolation').get('passed',False),
    'gpu_with_cpu_fencing_tested':evidence('cpu-gpu-compatibility').get('compute',{}).get('correct_result',False),
    'scratch_pool_removed':subprocess.run(['zpool','list','hcproof'],capture_output=True).returncode!=0,
    'cpu_configuration':'Active static lab prototype; register CPU policy before starting additional managed VMs',
    'iroh_implemented':False,'t3_server_installed':False,'hard_realtime_isolation_promised':False,
    'thin_provisioning_enabled_for_users':False,'temporary_passwordless_sudo_active':Path('/etc/sudoers.d/90-home-cloud-bootstrap').exists(),
}
assert all(result[k] for k in ['gpu_released','host_reboot_tested','multiuser_storage_exhaustion_tested',
                               'dedicated_cpu_workload_isolation_tested','gpu_with_cpu_fencing_tested','scratch_pool_removed'])
assert not result['recovery_timer_active']
(ROOT/'current-state.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result,indent=2))
