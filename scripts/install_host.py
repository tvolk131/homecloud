#!/usr/bin/python3
"""Explicit local-root installation onto an already configured Incus/ZFS host.

Never formats disks. The operator supplies a reviewed hardware policy JSON.
"""
import argparse
import host_deploy
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
    a=argparse.ArgumentParser();a.add_argument('--policy',required=True,type=Path);a.add_argument('--daemon',required=True,type=Path)
    a.add_argument('--cli',type=Path,help='Matching CLI (defaults to the daemon directory)')
    a.add_argument('--observer',type=Path,help='Matching Rust observer (defaults to the daemon directory)')
    a.add_argument('--check',action='store_true',help='Validate prerequisites and reviewed policy without installing')
    a.add_argument('--maintenance',action='store_true',help='Explicitly reinstall host policy; can interrupt management and workloads')
    args=a.parse_args()
    assert os.geteuid()==0,'Run locally with sudo'
    c=json.loads(args.policy.read_text())
    for program in ['incus','zfs','zpool','nft','gcc','fuser','lspci']:
        assert shutil.which(program),'Missing prerequisite: '+program
    if c.get('cloud_init_transport','agent')=='disk':assert shutil.which('mkisofs'),'Missing prerequisite: mkisofs (install genisoimage for cloud-init seed disks)'
    assert args.daemon.is_file()
    args.cli=args.cli or args.daemon.parent/'homecloud'
    assert args.cli.is_file(),'Supply the matching CLI with --cli'
    args.observer=args.observer or args.daemon.parent/'homecloud-host-observer'
    assert args.observer.is_file(),'Supply the matching Rust observer with --observer'
    assert args.observer.read_bytes()[:5]==b'\x7fELF\x02','Expected a Linux 64-bit observer'
    host_deploy.Services().check({'bin/homecloudd':args.daemon.read_bytes(),'bin/homecloud':args.cli.read_bytes(),
                                 'bin/homecloud-host-observer':args.observer.read_bytes()})
    # Complete read-only preflight before the first persistent change.
    assert args.daemon.read_bytes()[:5]==b'\x7fELF\x02','Expected a Linux 64-bit daemon'
    run(str(args.daemon.resolve()),'--help')
    groups=c['cpu_groups'];house=c['housekeeping'];flat=house+[cpu for group in groups for cpu in group]+[cpu for v in c.get('legacy_vms',[]) for cpu in v['cpu_ids']]
    assert groups and house and len(flat)==len(set(flat)),'Review complete, disjoint CPU topology'
    assert all(Path(f'/sys/devices/system/cpu/cpu{cpu}').exists() for cpu in flat),'Policy references an unavailable CPU'
    for group in groups:
        siblings=set()
        for part in Path(f'/sys/devices/system/cpu/cpu{group[0]}/topology/thread_siblings_list').read_text().strip().split(','):
            ends=part.split('-');siblings.update(range(int(ends[0]),int(ends[-1])+1))
        assert siblings==set(group),'CPU policy splits a physical core'
    assert c['dataset_root']==c['pool']+'/homecloud'
    assert c['capacity']['cpu_cores']==len(groups)
    assert all(type(v) is int and v>0 for v in c['capacity'].values())
    assert c['capacity']['memory_bytes']+c.get('minimum_reserves',{}).get('memory_bytes',0)<=os.sysconf('SC_PHYS_PAGES')*os.sysconf('SC_PAGE_SIZE'),'Memory policy exceeds installed RAM'
    health=subprocess.check_output(['zpool','status','-x',c['pool']],text=True);assert 'is healthy' in health,'Pool needs operator review'
    run('incus','image','info',c['image'])
    run('nft','--check','--file',str(ROOT/'crates/host/firewall.nft'))
    with host_deploy.tempfile.TemporaryDirectory(prefix='homecloud-install-check-') as temporary:
        run('gcc','-O2','-Wall','-Wextra','-Werror','-static','-o',str(Path(temporary)/'qemu'),str(ROOT/'crates/host/native/qemu_cpu_launcher.c'))
    if args.check:print('Installation preflight passed; no host policy or service changes.');return
    assert not Path('/etc/homecloud/host.json').exists() or args.maintenance,'Already installed. Use host_deploy.py for software upgrades, or --maintenance for an explicitly reviewed policy reinstall.'
    # Installer is deliberately a maintenance operation. Normal upgrades do not
    # enter this path or restart Incus. Interrupted install requires local repair.
    subprocess.run(['systemctl','stop','homecloudd.service'],check=False)
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
    host_deploy.atomic(Path(host_deploy.GUARD),(ROOT/'scripts/host_deploy.py').read_bytes(),0o755)
    helper=destination/'host_runtime.py';shutil.copyfile(ROOT/'crates/host/host_runtime.py',helper);helper.chmod(0o755)
    for source in [ROOT/'guest/setup-t3.sh', ROOT/'guest/managed-app.py', ROOT/'guest/setup-providers.py', ROOT/'guest/codex_auth.py', ROOT/'guest/claude_auth.py', ROOT/'guest/account_metadata.py', ROOT/'crates/host/managed_apps.py', ROOT/'crates/host/managed_host.py', ROOT/'crates/host/memory_capacity.py', ROOT/'guest/managed-host.py', ROOT/'crates/host/resource_reserves.py', ROOT/'crates/host/arc_policy.py', ROOT/'crates/host/instance_runtime.py']:
        shutil.copyfile(source,destination/source.name);(destination/source.name).chmod(0o644)
    for source in destination.glob('*.py'):host_deploy.clear_cache(source)
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
    run('install','-m','0755',str(args.cli),'/usr/local/bin/homecloud')
    run('install','-o','root','-g','root','-m','0755',str(args.observer),host_deploy.HELPER+'homecloud-host-observer')
    daemon={'limits':{'capacity':c['capacity'],'core_groups':c['cpu_groups'],'disk_overhead_gib_per_vm':c.get('disk_overhead_gib_per_vm',2)},'host_script':str(helper),'sudo':True}
    write(config/'control.json',json.dumps(daemon,indent=2),0o640)
    os.chown(config/'control.json',0,account.pw_gid)
    sudoers=('homecloud ALL=(root) NOPASSWD: /usr/bin/python3 /usr/local/libexec/homecloud/host_runtime.py\n'
             'homecloud ALL=(root) NOPASSWD: /usr/local/libexec/homecloud/homecloud-host-observer ""\n')
    temporary=config/'sudoers.check';write(temporary,sudoers,0o440);run('visudo','-cf',str(temporary))
    run('install','-m','0440',str(temporary),'/etc/sudoers.d/homecloud');temporary.unlink()
    house=','.join(str(x) for x in effective['housekeeping']);affinity=' '.join(str(x) for x in effective['housekeeping'])
    write('/etc/systemd/system/homecloud.slice','[Unit]\nDescription=Homecloud guest process fences\n[Slice]\nCPUAccounting=yes\n')
    for unit in ['system.slice','user.slice']:
        write('/etc/systemd/system/'+unit+'.d/homecloud.conf','[Slice]\nAllowedCPUs=\nAllowedCPUs='+house+'\n')
    write(config/'incus.env','PATH=/usr/local/libexec/homecloud/qemu:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n')
    legacy=' home-cloud-lab-cpu-layout.service' if c.get('legacy_vms') else ''
    write('/etc/systemd/system/homecloud-host-layout.service','[Unit]\nDescription=Homecloud CPU and guest network safety policy\nAfter=nftables.service'+legacy+'\nBefore=incus.service\n[Service]\nType=oneshot\nExecStartPre=+/usr/bin/python3 /usr/local/libexec/homecloud-deploy.py guard\nExecStart=/usr/bin/python3 /usr/local/libexec/homecloud/host_runtime.py cpu-layout\nExecStart=/usr/sbin/nft --file /etc/homecloud/firewall.nft\nRemainAfterExit=yes\n[Install]\nWantedBy=multi-user.target\n')
    write('/etc/systemd/system/incus.service.d/homecloud.conf','[Unit]\nRequires=homecloud-host-layout.service\nAfter=homecloud-host-layout.service\n[Service]\nKillMode=process\nCPUAffinity=\nCPUAffinity='+affinity+'\nEnvironmentFile=/etc/homecloud/incus.env\n')
    write('/etc/modules-load.d/homecloud.conf','br_netfilter\n')
    rules=(ROOT/'crates/host/firewall.nft').read_text();write(config/'firewall.nft',rules)
    run('modprobe','br_netfilter');run('nft','--check','--file',str(config/'firewall.nft'))
    write('/etc/systemd/system/homecloudd.service','[Unit]\nDescription=Homecloud control plane\nWants=network-online.target\nAfter=network-online.target\nWants=incus.service\n[Service]\nUser=homecloud\nGroup=homecloud\nUMask=0077\nExecStartPre=+/usr/bin/python3 /usr/local/libexec/homecloud-deploy.py guard\nExecStart=/usr/local/bin/homecloudd --config /etc/homecloud/control.json serve\nRestart=on-failure\nRestartSec=3\nEnvironment=RUST_LOG=homecloud_daemon=info\n[Install]\nWantedBy=multi-user.target\n')
    deployment=host_deploy.Deployment()
    with deployment.lock():deployment.register(ROOT)
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
