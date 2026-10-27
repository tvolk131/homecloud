"""Root-side fresh-guest proof. Only manages uniquely tagged proof VMs."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time
import uuid
from guest_cloud_init import cloud_config

TAG = 'provision-v1'
FINGERPRINT = '2139edd9da4026da8a66c0557af7eee4789101104eb27860bacd57086219cebb'


def run(*args, check=True, timeout=90):
    p = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if check and p.returncode:
        raise RuntimeError(f'{args}: {p.stdout}\n{p.stderr}')
    return p


def details(vm):
    return json.loads(run('incus', 'query', f'/1.0/instances/{vm}').stdout)


def guest(vm, *args, **kwargs):
    return run('incus', 'exec', vm, '--', *args, **kwargs)


def ready(vm, address):
    end = time.monotonic() + 600
    while time.monotonic() < end:
        p = guest(vm, 'cloud-init', 'status', '--format=json', check=False, timeout=15)
        if p.returncode == 0 and '"done"' in p.stdout:
            try:
                with socket.create_connection((address, 22), timeout=2):
                    return json.loads(p.stdout)
            except OSError:
                pass
        time.sleep(3)
    raise RuntimeError(guest(vm, 'tail', '-100', '/var/log/cloud-init-output.log', check=False).stdout)


assert os.geteuid() == 0
action, number = sys.argv[1], int(sys.argv[2])
assert number in (1, 2, 3)
vm = f'hclab-proof-provision-{number}'
address = f'10.77.2.{20 + number}'
if action == 'create':
    assert run('incus', 'query', f'/1.0/instances/{vm}', check=False).returncode != 0
    assert json.loads(run('incus', 'query', '/1.0/instances/hclab-vm02/state').stdout)['status'] == 'Stopped'
    public_key = sys.stdin.read().strip()
    run('incus', 'init', FINGERPRINT, vm, '--vm', '--profile', 'hclab',
        '-c', 'limits.cpu=4,10', '-c', f'user.home-cloud-proof={TAG}',
        '-c', f'cloud-init.user-data={cloud_config(public_key)}', timeout=300)
    run('incus', 'config', 'device', 'add', vm, 'eth0', 'nic', 'network=hcbr2',
        f'ipv4.address={address}', 'security.mac_filtering=true',
        'security.ipv4_filtering=true', 'security.ipv6_filtering=true')
    run('incus', 'config', 'device', 'add', vm, 'cloud-init', 'disk', 'source=cloud-init:config')
    run('incus', 'start', vm)
    cloud_status = ready(vm, address)
    config = guest(vm, 'sshd', '-T').stdout
    for setting in ('passwordauthentication no', 'kbdinteractiveauthentication no',
                    'pubkeyauthentication yes', 'permitrootlogin no', 'usepam yes'):
        assert setting in config, setting
    probe = r'''
import json,socket,urllib.request
r={}
with urllib.request.urlopen('https://github.com',timeout=20) as p:r['https']=p.status
r['dns']=bool(socket.getaddrinfo('github.com',443,family=socket.AF_INET))
for label,ip,port in [('host_lan','192.168.86.204',22),('host_bridge','10.77.2.1',22),('other_vm','10.77.1.10',22),('router','192.168.86.1',53)]:
 try:
  with socket.create_connection((ip,port),timeout=2):r[label]='reachable'
 except TimeoutError:r[label]='timeout'
 except OSError as e:r[label]=str(e)
print(json.dumps(r))
'''
    # Positive controls distinguish blocked connections from absent services.
    for ip, port in [('192.168.86.204',22),('10.77.2.1',22),('10.77.1.10',22),('192.168.86.1',53)]:
        with socket.create_connection((ip, port), timeout=3): pass
    network = json.loads(guest(vm, 'python3', '-c', probe, timeout=45).stdout)
    assert network['https'] == 200 and network['dns'], network
    assert all(network[k] == 'timeout' for k in ('host_lan','host_bridge','other_vm','router')), network
    d = details(vm)
    result = {'vm': vm, 'address': address, 'uuid': d['config']['volatile.uuid'],
              'image_fingerprint': d['config']['volatile.base_image'],
              'host_key': guest(vm, 'cat', '/etc/ssh/ssh_host_ed25519_key.pub').stdout.strip(),
              'cloud_init': cloud_status, 'network': network, 'ssh_configuration_verified': True,
              'manual_guest_repairs': False}
elif action == 'restart':
    assert details(vm)['config'].get('user.home-cloud-proof') == TAG
    before_key = guest(vm, 'cat', '/etc/ssh/ssh_host_ed25519_key.pub').stdout
    before_boot = guest(vm, 'cat', '/proc/sys/kernel/random/boot_id').stdout
    marker = str(uuid.uuid4())
    guest(vm, 'python3', '-c', 'from pathlib import Path;import os;p=Path("/home/dev/proof-marker");f=p.open("w");f.write(' + repr(marker) + ');f.flush();os.fsync(f.fileno());f.close()')
    run('incus', 'restart', vm, '--timeout', '30')
    ready(vm, address)
    result = {'data_retained': guest(vm, 'cat', '/home/dev/proof-marker').stdout == marker,
              'host_key_retained': guest(vm, 'cat', '/etc/ssh/ssh_host_ed25519_key.pub').stdout == before_key,
              'boot_id_changed': guest(vm, 'cat', '/proc/sys/kernel/random/boot_id').stdout != before_boot}
    assert all(result.values()), result
elif action == 'remove':
    assert details(vm)['config'].get('user.home-cloud-proof') == TAG
    run('incus', 'stop', vm, '--timeout', '30')
    run('incus', 'delete', vm)
    result = {'removed': vm}
else:
    raise ValueError(action)
print(json.dumps(result))
