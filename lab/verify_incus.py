#!/usr/bin/env python3
"""Exercise real guests and record the limits of the initial host prototype."""
import json
import os
from pathlib import Path
import socket
import subprocess
import time
import uuid


def run(*args, check=True, timeout=30):
    p = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if check and p.returncode:
        raise RuntimeError(f'{args!r}: {p.stdout}\n{p.stderr}')
    return p


def wait_ready(vm):
    deadline = time.monotonic() + 180
    last = None
    while time.monotonic() < deadline:
        last = run('incus', 'exec', vm, '--', 'cloud-init', 'status', check=False, timeout=15)
        if last.returncode == 0 and 'status: done' in last.stdout:
            run('incus', 'exec', vm, '--', 'id', '-u', 'dev')
            # Ubuntu may socket-activate sshd, so test the real listener.
            try:
                with socket.create_connection((f'10.77.{int(vm[-1])}.10', 22), timeout=2):
                    pass
                print(f'{vm}: cloud-init complete, dev account present, SSH reachable', flush=True)
                return
            except OSError:
                pass
        time.sleep(3)
    raise RuntimeError(f'{vm} not ready: {last.stdout}\n{last.stderr}')


assert os.geteuid() == 0
for vm in ('hclab-vm01', 'hclab-vm02'):
    wait_ready(vm)

guest_probe = r'''
import json, socket, urllib.request
results = {}
with urllib.request.urlopen('https://github.com', timeout=15) as response:
    results['public_https'] = response.status
results['dns'] = bool(socket.getaddrinfo('github.com', 443, family=socket.AF_INET))
targets = json.loads(__import__('sys').argv[1])
for label, (address, port) in targets.items():
    try:
        with socket.create_connection((address, port), timeout=2):
            results[label] = 'reachable'
    except TimeoutError:
        results[label] = 'timeout'
    except OSError as error:
        results[label] = str(error)
print(json.dumps(results))
'''
report = {'network': {}, 'cpu_pinning': {}, 'qemu_helper_affinity': {}, 'ssh': {}, 'failures': []}
for address, port in [('192.168.86.204', 22), ('10.77.1.10', 22), ('10.77.2.10', 22), ('192.168.86.1', 53)]:
    with socket.create_connection((address, port), timeout=5):
        pass
report['host_positive_control_ssh'] = True
report['lan_router_positive_control_dns'] = True

for number, cpus in ((1, {2, 8}), (2, {3, 9})):
    vm = f'hclab-vm0{number}'
    other = 2 if number == 1 else 1
    targets = {'host_lan_ssh': ['192.168.86.204', 22],
               'host_bridge_ssh': [f'10.77.{number}.1', 22],
               'other_vm_ssh': [f'10.77.{other}.10', 22],
               'lan_router_dns': ['192.168.86.1', 53]}
    result = json.loads(run('incus', 'exec', vm, '--', 'python3', '-c',
                            guest_probe, json.dumps(targets), timeout=45).stdout)
    report['network'][vm] = result
    if result.get('public_https') != 200 or not result.get('dns'):
        report['failures'].append(f'{vm}: internet or DNS unavailable')
    for target in targets:
        if result.get(target) != 'timeout':
            report['failures'].append(f'{vm}: unexpected result for {target}: {result.get(target)}')
    state = json.loads(run('incus', 'query', f'/1.0/instances/{vm}/state').stdout)
    threads = []
    helpers = []
    for task in (Path('/proc') / str(state['pid']) / 'task').iterdir():
        name = (task / 'comm').read_text().strip()
        allowed = next(line.split(':', 1)[1].strip()
                       for line in (task / 'status').read_text().splitlines()
                       if line.startswith('Cpus_allowed_list:'))
        if name.startswith('qemu') and allowed in {str(x) for x in cpus}:
            threads.append({'name': name, 'allowed_cpu': allowed})
        else:
            helpers.append({'name': name, 'allowed_cpu': allowed})
    report['cpu_pinning'][vm] = threads
    report['qemu_helper_affinity'][vm] = helpers
    if len(threads) != 2 or {t['allowed_cpu'] for t in threads} != {str(x) for x in cpus}:
        report['failures'].append(f'{vm}: unexpected vCPU affinity: {threads}')
    details = json.loads(run('incus', 'query', f'/1.0/instances/{vm}').stdout)
    report['ssh'][vm] = {
        'vm_uuid': details['config']['volatile.uuid'],
        'address': f'10.77.{number}.10',
        'host_public_key': run('incus', 'exec', vm, '--', 'cat',
                              '/etc/ssh/ssh_host_ed25519_key.pub').stdout.strip(),
        'image_fingerprint': details['config'].get('volatile.base_image'),
    }
    print(f'{vm}: network checks and vCPU affinity inspected', flush=True)

vm = 'hclab-vm01'
marker = str(uuid.uuid4())
write_marker = 'from pathlib import Path; import os; p=Path("/var/lib/home-cloud-lab-marker"); f=p.open("w"); f.write(' + repr(marker) + '); f.flush(); os.fsync(f.fileno()); f.close()'
run('incus', 'exec', vm, '--', 'python3', '-c', write_marker)
before_boot = run('incus', 'exec', vm, '--', 'cat', '/proc/sys/kernel/random/boot_id').stdout.strip()
run('incus', 'restart', vm, '--timeout', '30', timeout=90)
wait_ready(vm)
after_boot = run('incus', 'exec', vm, '--', 'cat', '/proc/sys/kernel/random/boot_id').stdout.strip()
after_marker = run('incus', 'exec', vm, '--', 'cat', '/var/lib/home-cloud-lab-marker').stdout.strip()
after_host_key = run('incus', 'exec', vm, '--', 'cat', '/etc/ssh/ssh_host_ed25519_key.pub').stdout.strip()
report['guest_restart'] = {'boot_id_changed': before_boot != after_boot,
                           'data_retained': marker == after_marker,
                           'ssh_host_key_retained': after_host_key == report['ssh'][vm]['host_public_key']}
if not all(report['guest_restart'].values()):
    report['failures'].append('Guest restart persistence check failed')

report['firewall'] = run('nft', 'list', 'table', 'inet', 'home_cloud_lab').stdout
report['storage'] = run('zfs', 'list', '-p', '-r', '-o',
    'name,type,used,available,referenced,volsize,refreservation', 'hclab').stdout
report['pool_health'] = run('zpool', 'status', 'hclab').stdout
report['packages'] = run('dpkg-query', '-W', '-f=${Package} ${Version}\n',
    'incus', 'qemu-system-x86', 'qemu-system-modules-spice', 'zfsutils-linux').stdout
report['firmware_dmar_present'] = Path('/sys/firmware/acpi/tables/DMAR').exists()
run('incus', 'stop', 'hclab-vm02', '--timeout', '30', timeout=90)
report['final_instances'] = [{'name': x['name'], 'status': x['status']}
    for x in json.loads(run('incus', 'list', '--format', 'json').stdout)]
Path('/var/lib/home-cloud-lab/verification.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))
raise SystemExit(bool(report['failures']))
