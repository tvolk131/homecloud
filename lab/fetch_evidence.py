#!/usr/bin/env python3
"""Copy lab evidence and pin guest SSH public keys learned through Incus."""
import json
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent.parent
access = root / '.host-access'
evidence = root / 'lab/evidence'
evidence.mkdir(exist_ok=True)
known_hosts = access / 'known_hosts'
key = Path.home() / '.ssh/id_ed25519'
ssh = ['/usr/bin/ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5',
       '-o', 'StrictHostKeyChecking=yes', '-o', f'UserKnownHostsFile={known_hosts}',
       '-o', 'IdentitiesOnly=yes', '-i', str(key), 'tommy@192.168.86.204']
documents = {}
for source, target in [('storage.json', 'storage.json'),
                       ('verification.json', 'verification.json'),
                       ('current-state.json', 'current-state.json'),
                       ('cloud-init-failure.json', 'cloud-init-failure.json'),
                       ('post-reboot.json', 'post-reboot.json'),
                       ('storage-guarantees.json', 'storage-guarantees.json'),
                       ('cpu-isolation.json', 'cpu-isolation.json'),
                       ('cpu-host-layout.json', 'cpu-host-layout.json'),
                       ('cpu-gpu-compatibility.json', 'cpu-gpu-compatibility.json'),
                       ('gpu-preflight.json', 'gpu-preflight.json'),
                       ('gpu-handoff-state.json', 'gpu-handoff-state.json'),
                       ('gpu-first-attach.json', 'gpu-first-attach.json'),
                       ('gpu-compute-first.json', 'gpu-compute-first.json'),
                       ('gpu-lifecycle.json', 'gpu-lifecycle.json')]:
    result = subprocess.run(ssh + [f'sudo -n cat /var/lib/home-cloud-lab/{source}'],
                            capture_output=True, text=True, check=True)
    document = json.loads(result.stdout)
    (evidence / target).write_text(json.dumps(document, indent=2) + '\n')
    documents[target] = document
report = documents['verification.json']
assert not report['failures'], report['failures']
text = known_hosts.read_text()
for vm, details in report['ssh'].items():
    alias = 'homecloud-' + details['vm_uuid']
    public_key = ' '.join(details['host_public_key'].split()[:2])
    matches = [line for line in text.splitlines()
               if line and not line.startswith('#') and alias in line.split()[0].split(',')]
    if matches:
        assert all(' '.join(line.split()[1:3]) == public_key for line in matches), \
            f'Guest host key changed for {vm}; explicit review required'
    else:
        text += f'{alias} {public_key}\n'
known_hosts.write_text(text)
known_hosts.chmod(0o600)
config_path = access / 'ssh_config'
config = f'''Host hclab-host
    HostName 192.168.86.204
    User tommy
    IdentityFile "{key}"
    IdentitiesOnly yes
    UserKnownHostsFile "{known_hosts}"
    StrictHostKeyChecking yes
    ConnectTimeout 10
    ServerAliveInterval 15
'''
for vm, details in report['ssh'].items():
    config += f'''
Host {vm}
    HostName {details['address']}
    User dev
    IdentityFile "{key}"
    IdentitiesOnly yes
    UserKnownHostsFile "{known_hosts}"
    HostKeyAlias homecloud-{details['vm_uuid']}
    StrictHostKeyChecking yes
    ConnectTimeout 10
    ServerAliveInterval 15
    ProxyCommand /usr/bin/ssh -F "{config_path}" -W %h:%p hclab-host
'''
config_path.write_text(config)
config_path.chmod(0o600)
print(json.dumps({'evidence_directory': str(evidence), 'ssh_config': str(config_path),
                  'guest_public_keys_pinned': list(report['ssh'])}, indent=2))
