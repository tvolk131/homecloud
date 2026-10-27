#!/usr/bin/env python3
"""Set up the fenced Incus prototype; no public daemon listener or guest ports."""
import json
import os
from pathlib import Path
import shutil
import subprocess
from guest_cloud_init import cloud_config


def run(*args, input=None, timeout=60):
    result = subprocess.run(args, input=input, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f'{args!r}: {result.stdout}\n{result.stderr}')
    return result.stdout


assert os.geteuid() == 0
for required in ('incus', 'nft', 'mkisofs', 'qemu-system-x86_64'):
    assert shutil.which(required), f'Missing host prerequisite: {required}'
assert run('dpkg-query', '-W', '-f=${db:Status-Status}',
           'qemu-system-modules-spice').strip() == 'installed'
public_key = os.environ['HC_LAB_SSH_PUBLIC_KEY']
assert public_key.startswith('ssh-ed25519 ') and '\n' not in public_key
assert run('zpool', 'list', '-H', '-o', 'health', 'hclab').strip() == 'ONLINE'
run('modprobe', 'br_netfilter')
Path('/etc/modules-load.d/home-cloud-lab.conf').write_text('br_netfilter\n')
root = Path('/etc/home-cloud-lab')
root.mkdir(mode=0o755, exist_ok=True)
firewall = '''add table inet home_cloud_lab
flush table inet home_cloud_lab
table inet home_cloud_lab {
  set non_public_v4 {
    type ipv4_addr
    flags interval
    elements = { 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8,
      169.254.0.0/16, 172.16.0.0/12, 192.0.0.0/24, 192.0.2.0/24,
      192.168.0.0/16, 198.18.0.0/15, 198.51.100.0/24, 203.0.113.0/24,
      224.0.0.0/4, 240.0.0.0/4 }
  }
  chain protect_host {
    type filter hook input priority -20; policy accept;
    iifname { "hcbr1", "hcbr2" } ct state established,related counter accept
    iifname { "hcbr1", "hcbr2" } meta nfproto ipv6 counter drop
    iifname { "hcbr1", "hcbr2" } udp dport { 53, 67 } counter accept
    iifname { "hcbr1", "hcbr2" } tcp dport 53 counter accept
    iifname { "hcbr1", "hcbr2" } counter drop
  }
  chain protect_network {
    type filter hook forward priority -20; policy accept;
    iifname { "hcbr1", "hcbr2" } meta nfproto ipv6 counter drop
    iifname { "hcbr1", "hcbr2" } ip daddr @non_public_v4 counter drop
    iifname { "hcbr1", "hcbr2" } oifname { "hcbr1", "hcbr2" } counter drop
    oifname { "hcbr1", "hcbr2" } ct state established,related counter accept
    oifname { "hcbr1", "hcbr2" } counter drop
  }
}
'''
rules = root / 'firewall.nft'
run('nft', '--check', '--file', '-', input=firewall)
rules.write_text(firewall)
unit = '''[Unit]
Description=Home Cloud lab guest network fence
After=nftables.service
Before=incus.service

[Service]
Type=oneshot
ExecStartPre=/usr/sbin/modprobe br_netfilter
ExecStart=/usr/sbin/nft --file /etc/home-cloud-lab/firewall.nft
ExecReload=/usr/sbin/nft --file /etc/home-cloud-lab/firewall.nft
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
'''
Path('/etc/systemd/system/home-cloud-lab-firewall.service').write_text(unit)
dropins = Path('/etc/systemd/system/incus.service.d')
dropins.mkdir(exist_ok=True)
(dropins / 'home-cloud-lab.conf').write_text('''[Unit]
Requires=home-cloud-lab-firewall.service
Wants=zfs-mount.service
After=home-cloud-lab-firewall.service zfs-mount.service
''')
run('systemctl', 'daemon-reload')
run('systemctl', 'enable', '--now', 'home-cloud-lab-firewall.service')
run('systemctl', 'reload', 'home-cloud-lab-firewall.service')
print('Persistent guest fence active; physical host interfaces unaffected', flush=True)

def names(resource):
    return {entry['name'] for entry in json.loads(run('incus', resource, 'list', '--format', 'json'))}

if 'hclab' not in names('storage'):
    run('incus', 'storage', 'create', 'hclab', 'zfs',
        'source=hclab/tenants/lab/incus',
        'volume.zfs.reserve_space=true', 'volume.zfs.use_refquota=true')
else:
    source = json.loads(run('incus', 'query', '/1.0/storage-pools/hclab'))['config']['source']
    assert source == 'hclab/tenants/lab/incus', source
print('Incus owns only hclab/tenants/lab/incus', flush=True)

for number in (1, 2):
    name = f'hcbr{number}'
    address = f'10.77.{number}.1/24'
    if name not in names('network'):
        run('incus', 'network', 'create', name, f'ipv4.address={address}',
            'ipv4.nat=true', 'ipv6.address=none', 'dns.mode=managed')
    else:
        config = json.loads(run('incus', 'query', f'/1.0/networks/{name}'))['config']
        assert config['ipv4.address'] == address and config['ipv6.address'] == 'none', config

if 'hclab' not in names('profile'):
    run('incus', 'profile', 'create', 'hclab')
    profile = {
        'description': 'Home Cloud disposable VM proving ground',
        'config': {'limits.memory': '2GiB', 'boot.autostart': 'false',
                   'user.home-cloud-lab': 'true'},
        'devices': {'root': {'type': 'disk', 'path': '/', 'pool': 'hclab', 'size': '20GiB'}}
    }
    run('incus', 'profile', 'edit', 'hclab', input=json.dumps(profile))

cloud_init = cloud_config(public_key)
for number, cores in ((1, '2,8'), (2, '3,9')):
    name = f'hclab-vm0{number}'
    existing = {vm['name']: vm for vm in json.loads(run('incus', 'list', '--format', 'json'))}
    if name not in existing:
        print(f'Creating {name}: Ubuntu 24.04 cloud VM, 2 GiB RAM, physical core threads {cores}', flush=True)
        run('incus', 'init', 'images:ubuntu/24.04/cloud', name, '--vm',
            '--profile', 'hclab', '-c', f'limits.cpu={cores}',
            '-c', 'user.home-cloud-lab=true', '-c', f'cloud-init.user-data={cloud_init}', timeout=900)
        run('incus', 'config', 'device', 'add', name, 'eth0', 'nic',
            f'network=hcbr{number}', f'ipv4.address=10.77.{number}.10',
            'security.mac_filtering=true', 'security.ipv4_filtering=true',
            'security.ipv6_filtering=true')
    else:
        assert existing[name]['config'].get('user.home-cloud-lab') == 'true', name
    state = json.loads(run('incus', 'query', f'/1.0/instances/{name}/state'))
    details = json.loads(run('incus', 'query', f'/1.0/instances/{name}'))
    if 'cloud-init' not in details['devices']:
        if state['status'] == 'Running':
            run('incus', 'stop', name, '--timeout', '30', timeout=90)
            state['status'] = 'Stopped'
        run('incus', 'config', 'device', 'add', name, 'cloud-init',
            'disk', 'source=cloud-init:config')
    if state['status'] == 'Stopped':
        run('incus', 'start', name, timeout=120)
    print(f'{name} started', flush=True)

evidence = Path('/var/lib/home-cloud-lab')
evidence.mkdir(exist_ok=True)
report = {
    'instances': json.loads(run('incus', 'list', '--format', 'json')),
    'storage': json.loads(run('incus', 'storage', 'list', '--format', 'json')),
    'networks': json.loads(run('incus', 'network', 'list', '--format', 'json')),
    'firewall': run('nft', 'list', 'table', 'inet', 'home_cloud_lab'),
}
(evidence / 'incus-setup.json').write_text(json.dumps(report, indent=2) + '\n')
print(run('incus', 'list'))
