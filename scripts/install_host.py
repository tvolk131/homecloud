#!/usr/bin/python3
"""Local-root installation and one-time metrics setup on an Incus/ZFS host.

Never formats disks. The operator supplies a reviewed hardware policy JSON.
"""
import argparse
import hashlib
import host_deploy
import importlib.util
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import subprocess
import sys
import time
import tempfile

ROOT=Path(__file__).resolve().parents[1]
# Unpacked bundles keep binaries in bin/; a source checkout builds the static
# guest runner for the musl target.
GUEST=ROOT/'bin/homecloud-guest' if (ROOT/'bin').is_dir() else ROOT/'target/x86_64-unknown-linux-musl/release/homecloud-guest'
def run(*args):subprocess.run(args,check=True)
def write(path,content,mode=0o644):
    path=Path(path);path.parent.mkdir(parents=True,exist_ok=True)
    path.write_text(content);path.chmod(mode)

def incus_query(path):
    return json.loads(subprocess.check_output(['incus','query',path],text=True))

def metrics_preflight(port=None,config_dir=Path('/etc/homecloud'),rotate=False):
    control=config_dir/'control.json'
    config=json.loads(control.read_text()) if control.exists() else {}
    metrics=config.get('metrics') or {}
    if port is None:port=metrics.get('port',8444)
    if type(port) is not int or not 1<=port<=65535:raise RuntimeError('Invalid metrics port')
    for program in ['incus','openssl','runuser']:
        if not shutil.which(program):raise RuntimeError('Missing prerequisite: '+program)
    server=incus_query('/1.0')
    settings=server['config'];address='127.0.0.1:'+str(port)
    if str(settings.get('core.metrics_authentication','true')).lower() not in ('','true'):
        raise RuntimeError('Refusing unauthenticated Incus metrics; enable core.metrics_authentication first')
    if settings.get('core.metrics_address','') not in ('',address):
        raise RuntimeError('An unrelated Incus metrics listener exists; review it manually instead of overwriting it')
    certificate=server['environment']['certificate'].encode()
    names=subprocess.check_output(['openssl','x509','-noout','-ext','subjectAltName'],input=certificate).decode()
    names=re.findall(r'DNS:([A-Za-z0-9][A-Za-z0-9.\-]{0,252})(?=,|\s|$)',names)
    if not names:raise RuntimeError('Incus server certificate needs a DNS subjectAltName for verified TLS')
    directory=config_dir/'metrics'
    if directory.is_symlink():raise RuntimeError('Metrics directory must not be a symlink')
    previous=metrics.get('client_identity')
    if previous and not rotate:
        source=Path(previous)
        if source.is_symlink() or not source.resolve().is_relative_to(directory.resolve()):
            raise RuntimeError('Refusing to reuse a metrics identity outside the managed credential directory')
        if not source.is_file():raise RuntimeError('Metrics identity is missing; restore its protected file or rerun with --rotate-metrics-client')
        try:subprocess.run(['openssl','x509','-in',str(source),'-checkend','604800','-noout'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        except subprocess.CalledProcessError as error:raise RuntimeError('Metrics certificate expires soon; rerun with --rotate-metrics-client') from error
    return address,certificate,names[0],bool(settings.get('core.metrics_address'))

def metrics_file(path,data,gid):
    descriptor,name=tempfile.mkstemp(prefix='.'+path.name+'-',dir=path.parent)
    try:
        with os.fdopen(descriptor,'wb') as output:
            os.fchmod(output.fileno(),0o640);os.fchown(output.fileno(),0,gid)
            output.write(data);output.flush();os.fsync(output.fileno())
        os.replace(name,path)
    finally:
        if os.path.exists(name):os.unlink(name)

def setup_metrics(daemon,port=None,check=False,rotate=False,config_dir=Path('/etc/homecloud')):
    address,server_certificate,server_name,had_listener=metrics_preflight(port,config_dir,rotate)
    port=int(address.rsplit(':',1)[1])
    control=config_dir/'control.json';config=json.loads(control.read_text())
    account=pwd.getpwnam('homecloud')
    if check:
        print('Metrics preflight passed; no listener, credentials or configuration changed.')
        return
    directory=config_dir/'metrics'
    if directory.is_symlink():raise RuntimeError('Metrics directory must not be a symlink')
    directory.mkdir(exist_ok=True);directory.chmod(0o750);os.chown(directory,0,account.pw_gid)
    registered=None;listener_changed=False;installed=None;committed=False
    try:
        with tempfile.TemporaryDirectory(prefix='.setup-',dir=directory) as temporary:
            stage=Path(temporary);stage.chmod(0o750);os.chown(stage,0,account.pw_gid)
            identity=stage/'client.pem';public=stage/'client.crt';key=stage/'client.key'
            previous=(config.get('metrics') or {}).get('client_identity')
            if previous and not rotate:
                source=Path(previous)
                if source.is_symlink() or not source.resolve().is_relative_to(directory.resolve()):
                    raise RuntimeError('Refusing to reuse a metrics identity outside the managed credential directory')
                metrics_file(identity,source.read_bytes(),account.pw_gid)
            else:
                subprocess.run(['openssl','req','-x509','-newkey','ec','-pkeyopt','ec_paramgen_curve:P-256',
                                '-nodes','-days','365','-subj','/CN=homecloud-metrics',
                                '-addext','basicConstraints=critical,CA:FALSE','-addext','extendedKeyUsage=clientAuth',
                                '-keyout',str(key),'-out',str(public)],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
                metrics_file(identity,public.read_bytes()+key.read_bytes(),account.pw_gid)
                key.unlink()
            certificate=subprocess.check_output(['openssl','x509','-in',str(identity),'-outform','PEM'])
            metrics_file(public,certificate,account.pw_gid)
            der=subprocess.check_output(['openssl','x509','-in',str(identity),'-outform','DER'])
            fingerprint=hashlib.sha256(der).hexdigest()
            trusted=incus_query('/1.0/certificates?recursion=1')
            matching=[entry for entry in trusted if entry['fingerprint']==fingerprint]
            if matching and any(entry['type']!='metrics' for entry in matching):
                raise RuntimeError('Refusing a management-capable client certificate; metrics-only trust is required')
            if not matching:
                registered=fingerprint
                run('incus','config','trust','add-certificate',str(public),'--type=metrics','--name=homecloud-metrics-'+fingerprint[:16])
            if not had_listener:
                listener_changed=True;run('incus','config','set','core.metrics_address',address)
            metrics_file(stage/'server.crt',server_certificate,account.pw_gid)
            staged=dict(config,metrics={'port':port,'server_name':server_name,'client_identity':str(identity),'server_certificate':str(stage/'server.crt')})
            metrics_file(stage/'control.json',json.dumps(staged).encode(),account.pw_gid)
            run('runuser','-u','homecloud','--',str(daemon.resolve()),'--config',str(stage/'control.json'),'check-metrics')
            version=directory/(fingerprint[:16]+'-'+hashlib.sha256(server_certificate).hexdigest()[:12])
            if version.is_symlink():raise RuntimeError('Metrics credential version must not be a symlink')
            if version.exists():
                for name in ['client.pem','server.crt']:
                    if (version/name).is_symlink() or (version/name).read_bytes()!=(stage/name).read_bytes():
                        raise RuntimeError('Existing metrics credential version differs; review it manually')
                version.chmod(0o750);os.chown(version,0,account.pw_gid)
                for name in ['client.pem','server.crt']:
                    (version/name).chmod(0o640);os.chown(version/name,0,account.pw_gid)
            else:
                version.mkdir(mode=0o750);os.chown(version,0,account.pw_gid);installed=version
                for name in ['client.pem','server.crt']:metrics_file(version/name,(stage/name).read_bytes(),account.pw_gid)
            config['metrics']={'port':port,'server_name':server_name,'client_identity':str(version/'client.pem'),'server_certificate':str(version/'server.crt')}
            metrics_file(control,(json.dumps(config,indent=2)+'\n').encode(),account.pw_gid)
            committed=True
    finally:
        if not committed:
            if listener_changed:subprocess.run(['incus','config','unset','core.metrics_address'],check=False)
            if registered:subprocess.run(['incus','config','trust','remove',registered],check=False)
            if installed:shutil.rmtree(installed)
    print('Verified loopback-only, metrics-only TLS access. Restart homecloudd.service to load it; no Incus or VM restart is needed.')
    if rotate:print('After restarting the controller, revoke the previous homecloud-metrics certificate and remove its old credential directory.')

def setup_metrics_and_resume(daemon,port,rotate,resume):
    try:setup_metrics(daemon,port,rotate=rotate)
    finally:
        try:
            for name in resume:
                if incus_query('/1.0/instances/'+name+'/state')['status']!='Running':run('incus','start',name)
        finally:run('systemctl','restart','homecloudd.service')

def main():
    a=argparse.ArgumentParser();a.add_argument('--policy',type=Path);a.add_argument('--daemon',type=Path)
    a.add_argument('--cli',type=Path,help='Matching CLI (defaults to the daemon directory)')
    a.add_argument('--guest',type=Path,default=GUEST,help='Static guest runner pushed into managed VMs')
    a.add_argument('--check',action='store_true',help='Validate prerequisites and reviewed policy without installing')
    a.add_argument('--maintenance',action='store_true',help='Explicitly reinstall host policy; can interrupt management and workloads')
    a.add_argument('--metrics-only',action='store_true',help='Provision authenticated loopback Incus metrics on an existing installation')
    a.add_argument('--metrics-port',type=int,help='Metrics listener port (existing configured port, or 8444 on first setup)')
    a.add_argument('--rotate-metrics-client',action='store_true',help='Issue a new metrics-only client identity; retain the previous identity until controller restart')
    args=a.parse_args()
    assert os.geteuid()==0,'Run locally with sudo'
    if args.metrics_only:
        if args.check:setup_metrics(args.daemon or Path('/usr/local/bin/homecloudd'),args.metrics_port,check=True,rotate=args.rotate_metrics_client)
        else:
            with host_deploy.Deployment().lock():
                setup_metrics(args.daemon or Path('/usr/local/bin/homecloudd'),args.metrics_port,rotate=args.rotate_metrics_client)
        return
    if args.policy is None or args.daemon is None:a.error('--policy and --daemon are required unless --metrics-only is used')
    metrics_preflight(args.metrics_port,rotate=args.rotate_metrics_client)
    c=json.loads(args.policy.read_text())
    for program in ['incus','zfs','zpool','nft','gcc','fuser','lspci']:
        assert shutil.which(program),'Missing prerequisite: '+program
    if c.get('cloud_init_transport','agent')=='disk':assert shutil.which('mkisofs'),'Missing prerequisite: mkisofs (install genisoimage for cloud-init seed disks)'
    assert args.daemon.is_file()
    args.cli=args.cli or args.daemon.parent/'homecloud'
    assert args.cli.is_file(),'Supply the matching CLI with --cli'
    assert args.guest.is_file(),'Build the guest runner (see docs/deployment.md) or supply it with --guest'
    assert host_deploy.static_elf(args.guest.read_bytes()),'Expected a static Linux x86-64 guest runner; build it for x86_64-unknown-linux-musl'
    host_deploy.Services().check({'bin/homecloudd':args.daemon.read_bytes(),'bin/homecloud':args.cli.read_bytes(),
                                  host_deploy.GUEST_RUNNER:args.guest.read_bytes()})
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
    run('install','-m','0755',str(args.guest),str(destination/'homecloud-guest'))
    for name in ['setup-t3.sh','managed-app.py','setup-providers.py','codex_auth.py','claude_auth.py','github_auth.py','account_metadata.py']:
        (destination/name).unlink(missing_ok=True)  # Replaced by the guest runner.
    for source in [ROOT/'crates/host/managed_apps.py', ROOT/'crates/host/managed_host.py', ROOT/'crates/host/memory_capacity.py', ROOT/'guest/managed-host.py', ROOT/'crates/host/resource_reserves.py', ROOT/'crates/host/arc_policy.py', ROOT/'crates/host/instance_runtime.py']:
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
    daemon={'limits':{'capacity':c['capacity'],'core_groups':c['cpu_groups'],'disk_overhead_gib_per_vm':c.get('disk_overhead_gib_per_vm',2)},'host_script':str(helper),'sudo':True}
    if (config/'control.json').exists():
        previous_metrics=json.loads((config/'control.json').read_text()).get('metrics')
        if previous_metrics:daemon['metrics']=previous_metrics
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
    setup_metrics_and_resume(Path('/usr/local/bin/homecloudd'),args.metrics_port,args.rotate_metrics_client,resume)
    deadline=time.monotonic()+35
    while time.monotonic()<deadline:
        if subprocess.run(['systemctl','is-active','--quiet','homecloudd']).returncode==0 and (state/'server.json').exists():break
        time.sleep(.5)
    else:raise RuntimeError('Controller did not become ready; inspect journalctl -u homecloudd. Installed files and VM data were retained.')
    print('Installed. Existing disks were retained. Local-root bootstrap command:')
    print('sudo -u homecloud homecloudd --config /etc/homecloud/control.json bootstrap-admin --name Administrator')

if __name__=='__main__':main()
