"""Resumable, bounded Proxmox experiment on the pinned resolvr-box lab host."""

import argparse
import fcntl
import json
from pathlib import Path
import shlex
import subprocess
import time
import uuid

from control_runner import atomic_json

ROOT = Path(__file__).resolve().parents[1]


HOST_PROGRAM = r'''
import json,pathlib,subprocess,sys,time
request=json.load(sys.stdin)
state=request['state'];action=request['action'];token=state['run_id']
if len(token)!=32 or any(character not in '0123456789abcdef' for character in token):
 raise RuntimeError('Invalid run identity')
name=state['instance'];pool=state['storage'];network=state['network'];dataset=state['dataset']
if name!='hc-pve-'+token[:10] or pool!='hcpve-'+token[:10] or network!='hcvmsp'+token[:8] or dataset!='hclab/proxmox-spike-'+token:
 raise RuntimeError('Resource identity mismatch')
directory=pathlib.Path('/var/lib/homecloud/test-runs')/('proxmox-'+token)

def run(*arguments,check=True,timeout=120,input=None):
 result=subprocess.run(arguments,input=input,text=True,capture_output=True,timeout=timeout)
 if check and result.returncode:
  raise RuntimeError(' '.join(arguments[:3])+': '+(result.stderr or result.stdout)[-1800:])
 return result

def instance():
 result=run('incus','query','/1.0/instances/'+name,check=False)
 if result.returncode:return None
 value=json.loads(result.stdout)
 if value['config'].get('user.homecloud.spike')!=token:raise RuntimeError('Foreign VM')
 return value

def owned_dataset():
 result=run('zfs','get','-H','-o','value','org.homecloud:spike',dataset,check=False)
 if result.returncode:return False
 if result.stdout.strip()!=token:raise RuntimeError('Foreign dataset')
 return True

def guest(arguments,input=None,timeout=120,check=True):
 if instance() is None:raise RuntimeError('Missing worker')
 return run('incus','exec',name,'--',*arguments,input=input,timeout=timeout,check=check)

def wait_agent():
 deadline=time.monotonic()+240
 while time.monotonic()<deadline:
  result=run('incus','exec',name,'--','true',check=False,timeout=15)
  if result.returncode==0:return
  time.sleep(3)
 raise TimeoutError('Worker agent did not become available')

if action=='prepare':
 directory.mkdir(parents=True,exist_ok=True,mode=0o700)
 (directory/'manifest.json').write_text(json.dumps(state,indent=2)+'\n')
 (directory/'manifest.json').chmod(0o600)
 if run('systemctl','is-active','homecloudd',check=False).returncode==0:
  run('systemctl','stop','homecloudd')
 if run('zpool','list','-H','-o','health','hclab').stdout.strip()!='ONLINE':raise RuntimeError('Unhealthy parent pool')
 if not owned_dataset():
  available=int(run('zfs','get','-Hp','-o','value','available','hclab').stdout)
  if available<200*1024**3:raise RuntimeError('Insufficient unreserved parent capacity')
  run('zfs','create','-o','quota=128G','-o','reservation=128G','-o','mountpoint=none','-o','org.homecloud:spike='+token,dataset)
 if run('incus','storage','show',pool,check=False).returncode:
  run('incus','storage','create',pool,'zfs','source='+dataset+'/incus','user.homecloud.spike='+token)
 else:
  value=json.loads(run('incus','query','/1.0/storage-pools/'+pool).stdout)
  if value['config'].get('user.homecloud.spike')!=token:raise RuntimeError('Foreign storage')
 if run('incus','network','show',network,check=False).returncode:
  run('incus','network','create',network,'ipv4.address=172.31.246.1/24','ipv4.nat=true','ipv6.address=none','user.homecloud.spike='+token)
 if instance() is None:
  run('incus','init','images:debian/13/cloud',name,'--vm','--no-profiles','--storage',pool,'--network',network,
      '-c','limits.memory=12GiB','-c','limits.cpu=4','-c','security.secureboot=false','-c','boot.autostart=false',
      '-c','raw.apparmor=/usr/bin/qemu-system-x86_64 rix,','-c','user.homecloud.spike='+token,timeout=600)
  run('incus','config','device','set',name,'root','size=32GiB')
  run('incus','config','device','set',name,'eth0','ipv4.address=172.31.246.2')
  run('incus','storage','volume','create',pool,'scratch','--type=block','size=64GiB','user.homecloud.spike='+token)
  run('incus','config','device','add',name,'scratch','disk','pool='+pool,'source=scratch')
 unit='homecloud-'+token+'.service'
 policy_path=pathlib.Path('/etc/homecloud/cpu-policy.tsv')
 backup=directory/'cpu-policy.original'
 if not backup.exists():
  backup.write_bytes(policy_path.read_bytes());backup.chmod(0o600)
 housekeeping=json.loads(pathlib.Path('/etc/homecloud/host.json').read_text())['housekeeping']
 reserve_path=pathlib.Path('/var/lib/homecloud/host/resource-reserves.json')
 if reserve_path.exists():
  reserves=json.loads(reserve_path.read_text())
  housekeeping += [cpu for group in reserves['state']['core_groups'] for cpu in group]
 cpus=','.join(str(cpu) for cpu in sorted(set(housekeeping)))
 entry=f'{name}\t{cpus}\t{cpus}\t/sys/fs/cgroup/homecloud.slice/{unit}\n'.encode()
 original=backup.read_bytes()
 if policy_path.read_bytes() not in (original,original+entry):raise RuntimeError('CPU policy changed outside spike')
 if run('systemctl','is-active',unit,check=False).returncode:
  run('systemd-run','--quiet','--collect','--unit='+unit,'--slice=homecloud.slice',
      '--property=AllowedCPUs='+cpus,'--property=CPUAffinity='+cpus.replace(',',' '),'/usr/bin/sleep','infinity')
 policy_path.write_bytes(original+entry)
 apparmor=f'/usr/bin/qemu-system-x86_64 rix,\n/etc/homecloud/cpu-policy.tsv r,\n/sys/fs/cgroup/homecloud.slice/{unit}/cgroup.procs w,\n/sys/fs/cgroup/homecloud.slice/{unit}/cpuset.cpus.effective r,\n/var/log/homecloud-cpu-launch.jsonl w,\n'
 run('incus','config','set',name,'raw.apparmor='+apparmor)
 if instance()['status']!='Running':run('incus','start',name)
 wait_agent()
 print(json.dumps({'prepared':True,'instance':name,'nested':guest(['sh','-c','test -c /dev/kvm && grep -qw vmx /proc/cpuinfo && echo yes']).stdout.strip(),
                   'parent_accounting':run('zfs','list','-Hp','-o','name,used,available,quota,reservation',dataset).stdout.strip()}))
elif action=='reboot':
 if instance()['status']=='Running':run('incus','stop',name,'--timeout','120',timeout=150)
 run('incus','start',name)
 wait_agent()
 print(json.dumps({'rebooted':True,'kernel':guest(['uname','-r']).stdout.strip()}))
elif action=='guest':
 result=guest(['python3','-',request['guest_action'],token],input=request['program'],timeout=request.get('timeout',120))
 print(result.stdout.strip())
elif action=='cleanup':
 forced=False
 value=instance()
 if value is not None:
  if value['status']!='Stopped':
   stopped=run('incus','stop',name,'--timeout','120',timeout=150,check=False)
   if stopped.returncode:
    if not request.get('force_worker_stop'):raise RuntimeError('Disposable worker did not stop gracefully: '+stopped.stderr[-1200:])
    if instance() is not None:
     run('incus','stop',name,'--force')
     forced=True
  run('incus','delete',name)
 storage=run('incus','query','/1.0/storage-pools/'+pool,check=False)
 if storage.returncode==0:
  value=json.loads(storage.stdout)
  if value['config'].get('user.homecloud.spike')!=token:raise RuntimeError('Foreign storage')
  volume=run('incus','storage','volume','show',pool,'scratch',check=False)
  if volume.returncode==0:run('incus','storage','volume','delete',pool,'scratch')
  run('incus','storage','delete',pool)
 network_result=run('incus','query','/1.0/networks/'+network,check=False)
 if network_result.returncode==0:
  value=json.loads(network_result.stdout)
  if value['config'].get('user.homecloud.spike')!=token:raise RuntimeError('Foreign network')
  run('incus','network','delete',network)
 if owned_dataset():run('zfs','destroy','-r',dataset)
 backup=directory/'cpu-policy.original'
 if backup.exists():
  policy_path=pathlib.Path('/etc/homecloud/cpu-policy.tsv')
  current=policy_path.read_bytes()
  restored=b''.join(line for line in current.splitlines(keepends=True) if not line.startswith((name+'\t').encode()))
  if restored!=backup.read_bytes():raise RuntimeError('Refusing to overwrite externally changed CPU policy')
  policy_path.write_bytes(restored)
  run('systemctl','stop','homecloud-'+token+'.service',check=False)
 if state['controller_was_active']:run('systemctl','start','homecloudd')
 controller=run('systemctl','is-active','homecloudd',check=False).stdout.strip()
 if state['controller_was_active'] and controller!='active':raise RuntimeError('Original controller did not restart')
 remaining={kind:json.loads(run('incus',kind,'list','--format=json').stdout) for kind in ('storage','network')}
 if any(entry['name']==pool for entry in remaining['storage']):raise RuntimeError('Lab storage remains')
 if any(entry['name']==network for entry in remaining['network']):raise RuntimeError('Lab network remains')
 if instance() is not None:raise RuntimeError('Lab instance remains')
 datasets=run('zfs','list','-H','-o','name','-r','hclab').stdout.splitlines()
 if any(entry==dataset or entry.startswith(dataset+'/') for entry in datasets):raise RuntimeError('Lab dataset remains')
 health=run('zpool','list','-H','-o','health','hclab').stdout.strip()
 if health!='ONLINE':raise RuntimeError('Physical pool is not healthy after cleanup')
 print(json.dumps({'cleaned':True,'controller_active':controller,'lab_resources_absent':True,'forced_worker_stop':forced,
                   'original_cpu_policy_restored':backup.exists() and policy_path.read_bytes()==backup.read_bytes(),
                   'physical_pool_health':health}))
else:raise RuntimeError('Unknown host action')
'''


def remote(args, request, timeout=180):
    command = shlex.join(['sudo', '-n', 'python3', '-c', HOST_PROGRAM])
    result = subprocess.run(
        ['ssh', '-F', args.ssh_config, '-o', 'BatchMode=yes', 'hclab-host', command],
        input=json.dumps(request), text=True, capture_output=True, timeout=timeout,
    )
    if result.returncode:
        raise RuntimeError(result.stderr[-3000:] or result.stdout[-3000:])
    return json.loads(result.stdout)


def snapshot(args):
    result = subprocess.run([args.cli, 'status'], text=True, capture_output=True, timeout=60)
    if result.returncode:
        raise RuntimeError(result.stderr)
    response = json.loads(result.stdout)
    if response['kind'] != 'snapshot':
        raise RuntimeError('Unexpected control response: ' + response['kind'])
    value = response['data']
    return {
        'controller_was_active': True,
        'original_vms': [{'id': vm['id'], 'name': vm['spec']['name'], 'desired': vm['desired'], 'observed': vm['observed']}
                         for vm in value['vms']],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['prepare', 'install', 'reboot', 'probe', 'storage', 'templates', 't3', 'cleanup'])
    parser.add_argument('--state', required=True)
    parser.add_argument('--ssh-config', default=str(Path.home() / '.config/homecloud/host-access/resolvr-box/ssh_config'))
    parser.add_argument('--cli', default=str(ROOT / 'target/debug/homecloud'))
    parser.add_argument('--force-worker-stop', action='store_true', help='Allow cleanup to force-stop only the tagged disposable worker after graceful shutdown fails')
    args = parser.parse_args()
    if args.force_worker_stop and args.action != 'cleanup':
        parser.error('--force-worker-stop is only valid with cleanup')
    path = Path(args.state)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (path.parent / '.homecloud-lab.lock').open('a') as run_lock:
        try:
            fcntl.flock(run_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('Another operation is using this spike manifest') from None
        execute(args, path)


def execute(args, path):
    if path.exists():
        state = json.loads(path.read_text())
        if state.get('cleaned') and args.action != 'cleanup':
            raise RuntimeError('This run was cleaned; use a new manifest for another experiment')
    else:
        if args.action != 'prepare':
            raise RuntimeError('Prepare a new run first')
        token = uuid.uuid4().hex
        state = {
            'version': 1, 'run_id': token, 'instance': 'hc-pve-' + token[:10],
            'storage': 'hcpve-' + token[:10], 'network': 'hcvmsp' + token[:8],
            'dataset': 'hclab/proxmox-spike-' + token, 'observations': {},
            **snapshot(args),
        }
        atomic_json(path, state)
    request = {'action': args.action, 'state': state, 'force_worker_stop': args.force_worker_stop}
    timeout = 900
    if args.action in ('install', 'probe', 'storage', 'templates', 't3'):
        timeout = 1800 if args.action == 'install' else 1200
        assets = {name: (ROOT / 'guest' / name).read_text() for name in ('setup-t3.sh', 'setup-providers.py')}
        request.update(action='guest', guest_action=args.action,
                       program='ASSETS = ' + repr(assets) + '\n' + (Path(__file__).with_name('proxmox_guest.py')).read_text(), timeout=timeout)
    try:
        result = remote(args, request, timeout=timeout + 30)
        if args.action == 'cleanup':
            restored = snapshot(args)['original_vms']
            expected = {entry['id']: entry for entry in state['original_vms']}
            actual = {entry['id']: entry for entry in restored}
            if actual != expected:
                raise RuntimeError('Original workload state changed; inspect before claiming restoration')
            result['original_workloads_unchanged'] = True
        state['observations'][args.action] = result
        state.pop('failure', None)
        if args.action == 'cleanup':
            state['cleaned'] = True
        atomic_json(path, state)
        print(json.dumps(result, indent=2))
    except BaseException as error:
        failure = {'action': args.action, 'error': str(error), 'time': time.time()}
        state['failure'] = failure
        state.setdefault('failures', []).append(failure)
        atomic_json(path, state)
        raise


if __name__ == '__main__':
    main()
