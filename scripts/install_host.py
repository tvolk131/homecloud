#!/usr/bin/python3
"""Explicit local-root installation onto an already configured Incus/ZFS host.

Never formats disks. The operator supplies a reviewed hardware policy JSON.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import sys
import time

ROOT=Path(__file__).resolve().parents[1]
def run(*args):subprocess.run(args,check=True)
def write(path,content,mode=0o644):
    path=Path(path);path.parent.mkdir(parents=True,exist_ok=True)
    path.write_text(content);path.chmod(mode)

def main():
    a=argparse.ArgumentParser();a.add_argument('--policy',required=True,type=Path);a.add_argument('--daemon',required=True,type=Path);args=a.parse_args()
    assert os.geteuid()==0,'Run locally with sudo'
    c=json.loads(args.policy.read_text())
    for program in ['incus','zfs','zpool','nft','gcc','fuser','lspci']:
        assert shutil.which(program),'Missing prerequisite: '+program
    if c.get('cloud_init_transport','agent')=='disk':assert shutil.which('mkisofs'),'Missing prerequisite: mkisofs (install genisoimage for cloud-init seed disks)'
    assert args.daemon.is_file()
    destination=Path('/usr/local/libexec/homecloud');destination.mkdir(parents=True,exist_ok=True)
    config=Path('/etc/homecloud');config.mkdir(exist_ok=True);config.chmod(0o755)
    backup=Path('/var/lib/homecloud/install-backups')/str(int(time.time()));backup.mkdir(parents=True)
    for path in [config,Path('/etc/systemd/system/incus.service.d'),Path('/etc/systemd/system/system.slice.d'),Path('/etc/systemd/system/user.slice.d')]:
        if path.exists():shutil.copytree(path,backup/path.name,dirs_exist_ok=True)
    try:account=pwd.getpwnam('homecloud')
    except KeyError:
        run('useradd','--system','--home-dir','/var/lib/homecloud/control','--shell','/usr/sbin/nologin','homecloud');account=pwd.getpwnam('homecloud')
    Path('/var/lib/homecloud').mkdir(parents=True,exist_ok=True);Path('/var/lib/homecloud').chmod(0o755)
    config.chmod(0o750);os.chown(config,0,account.pw_gid)
    state=Path('/var/lib/homecloud/control');state.mkdir(parents=True,exist_ok=True);state.chmod(0o700);os.chown(state,account.pw_uid,account.pw_gid)
    hoststate=Path('/var/lib/homecloud/host');hoststate.mkdir(exist_ok=True);hoststate.chmod(0o700)
    write(config/'host.json',json.dumps(c,indent=2),0o600)
    helper=destination/'host_runtime.py';shutil.copyfile(ROOT/'crates/host/host_runtime.py',helper);helper.chmod(0o755)
    for source in [ROOT/'guest/setup-t3.sh', ROOT/'guest/managed-app.py', ROOT/'guest/setup-providers.py', ROOT/'guest/codex_auth.py', ROOT/'guest/claude_auth.py', ROOT/'guest/account_metadata.py', ROOT/'crates/host/managed_apps.py', ROOT/'crates/host/managed_host.py', ROOT/'crates/host/memory_capacity.py', ROOT/'guest/managed-host.py', ROOT/'crates/host/resource_reserves.py', ROOT/'crates/host/arc_policy.py', ROOT/'crates/host/instance_runtime.py']:
        shutil.copyfile(source,destination/source.name);(destination/source.name).chmod(0o644)
    spec=importlib.util.spec_from_file_location('homecloud_host',helper);module=importlib.util.module_from_spec(spec);sys.modules[spec.name]=module;spec.loader.exec_module(module)
    effective=module.load_config() # validates topology and retains the durable reserve overlay
    if module.resource_reserves.arc_policy.policy(effective) is not None:
        # Fresh profiles start with a positive, reboot-persistent ceiling before
        # the controller can issue any capacity. Existing journal intent wins.
        mode=effective['_reserve_record']['state'].get('arc') or {'mode':'automatic'}
        module.resource_reserves.arc_policy.apply(module,effective,mode)
    run('zpool','status',c['pool'])
    (destination/'qemu').mkdir(exist_ok=True)
    run('gcc','-O2','-Wall','-Wextra','-Werror','-static','-o',str(destination/'qemu/qemu-system-x86_64'),str(ROOT/'crates/host/native/qemu_cpu_launcher.c'))
    audit=Path('/var/log/homecloud-cpu-launch.jsonl');audit.touch(exist_ok=True);audit.chmod(0o600)
    run('install','-m','0755',str(args.daemon),'/usr/local/bin/homecloudd')
    daemon={'limits':{'capacity':c['capacity'],'core_groups':c['cpu_groups'],'disk_overhead_gib_per_vm':c.get('disk_overhead_gib_per_vm',2)},'host_script':str(helper),'sudo':True}
    write(config/'control.json',json.dumps(daemon,indent=2),0o640)
    os.chown(config/'control.json',0,account.pw_gid)
    sudoers='homecloud ALL=(root) NOPASSWD: /usr/bin/python3 /usr/local/libexec/homecloud/host_runtime.py\n'
    temporary=config/'sudoers.check';write(temporary,sudoers,0o440);run('visudo','-cf',str(temporary))
    run('install','-m','0440',str(temporary),'/etc/sudoers.d/homecloud');temporary.unlink()
    house=','.join(str(x) for x in effective['housekeeping']);affinity=' '.join(str(x) for x in effective['housekeeping'])
    write('/etc/systemd/system/homecloud.slice','[Unit]\nDescription=Homecloud guest process fences\n[Slice]\nCPUAccounting=yes\n')
    for unit in ['system.slice','user.slice']:
        write('/etc/systemd/system/'+unit+'.d/homecloud.conf','[Slice]\nAllowedCPUs=\nAllowedCPUs='+house+'\n')
    write(config/'incus.env','PATH=/usr/local/libexec/homecloud/qemu:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n')
    legacy=' home-cloud-lab-cpu-layout.service' if c.get('legacy_vms') else ''
    write('/etc/systemd/system/homecloud-host-layout.service','[Unit]\nDescription=Homecloud CPU and guest network safety policy\nAfter=nftables.service'+legacy+'\nBefore=incus.service\n[Service]\nType=oneshot\nExecStart=/usr/bin/python3 /usr/local/libexec/homecloud/host_runtime.py cpu-layout\nExecStart=/usr/sbin/nft --file /etc/homecloud/firewall.nft\nRemainAfterExit=yes\n[Install]\nWantedBy=multi-user.target\n')
    write('/etc/systemd/system/incus.service.d/homecloud.conf','[Unit]\nRequires=homecloud-host-layout.service\nAfter=homecloud-host-layout.service\n[Service]\nKillMode=process\nCPUAffinity=\nCPUAffinity='+affinity+'\nEnvironmentFile=/etc/homecloud/incus.env\n')
    write('/etc/modules-load.d/homecloud.conf','br_netfilter\n')
    rules=(ROOT/'crates/host/firewall.nft').read_text();write(config/'firewall.nft',rules)
    run('modprobe','br_netfilter');run('nft','--check','--file',str(config/'firewall.nft'))
    write('/etc/systemd/system/homecloudd.service','[Unit]\nDescription=Homecloud control plane\nWants=network-online.target\nAfter=network-online.target\nWants=incus.service\n[Service]\nUser=homecloud\nGroup=homecloud\nUMask=0077\nExecStart=/usr/local/bin/homecloudd --config /etc/homecloud/control.json serve\nRestart=on-failure\nRestartSec=3\nEnvironment=RUST_LOG=homecloud_daemon=info\n[Install]\nWantedBy=multi-user.target\n')
    run('systemctl','daemon-reload')
    module.cpu_policy(effective,json.loads((hoststate/'cpu-vms.json').read_text()) if (hoststate/'cpu-vms.json').exists() else [])
    resume=[]
    for vm in c.get('legacy_vms',[]):
        details=module.api('GET','/1.0/instances/'+vm['name'])
        assert details['config'].get('user.home-cloud-lab')=='true','Foreign legacy instance'
        unit=vm['unit'];aa=f'/etc/homecloud/cpu-policy.tsv r,\n/sys/fs/cgroup/homecloud.slice/{unit}/cgroup.procs w,\n/sys/fs/cgroup/homecloud.slice/{unit}/cpuset.cpus.effective r,\n/var/log/homecloud-cpu-launch.jsonl w,\n'
        if aa not in details['config'].get('raw.apparmor',''):
            if module.api('GET','/1.0/instances/'+vm['name']+'/state')['status']=='Running':
                run('incus','stop',vm['name'],'--timeout','30');resume.append(vm['name'])
            run('incus','config','set',vm['name'],'raw.apparmor',details['config'].get('raw.apparmor','')+'\n'+aa)
    for unit in ['system.slice','user.slice']:run('systemctl','set-property','--runtime',unit,'AllowedCPUs='+house)
    run('systemctl','enable','homecloud-host-layout.service','homecloudd.service')
    run('systemctl','restart','homecloud-host-layout.service')
    run('systemctl','restart','incus.service')
    for name in resume:
        if module.api('GET','/1.0/instances/'+name+'/state')['status']!='Running':run('incus','start',name)
    run('systemctl','restart','homecloudd.service')
    deadline=time.monotonic()+35
    while time.monotonic()<deadline:
        if subprocess.run(['systemctl','is-active','--quiet','homecloudd']).returncode==0 and (state/'server.json').exists():break
        time.sleep(.5)
    else:raise RuntimeError('Controller did not become ready; inspect journalctl -u homecloudd. Installed files and VM data were retained.')
    print('Installed. Existing disks were retained. Local-root bootstrap command:')
    print('sudo -u homecloud homecloudd --config /etc/homecloud/control.json bootstrap-admin --name Administrator')

if __name__=='__main__':main()
