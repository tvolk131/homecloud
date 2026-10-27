"""Mac-side fresh provisioning proof with authenticated SSH and SFTP."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
from remote import ACCESS, ROOT, save, script, upload

upload('guest_cloud_init.py', 'provisioning_guest.py')
public_key = (Path.home() / '.ssh/id_ed25519.pub').read_text().strip()
report = {'guests': [], 'transport': 'OpenSSH via host; Iroh deferred',
          'started_at': time.time(), 'passed': False}
for number in (1, 2, 3):
    print(f'Creating fresh guest {number}/3; waiting for cloud-init', flush=True)
    entry = json.loads(script('provisioning_guest.py', 'create', number,
                              input=public_key, timeout=750))
    report['guests'].append(entry)
    save('provisioning.json', report)
    alias = 'homecloud-' + entry['uuid']
    with tempfile.TemporaryDirectory(prefix='hclab-provision-') as tmp:
        tmp = Path(tmp)
        known = tmp / 'known_hosts'
        known.write_text(alias + ' ' + ' '.join(entry['host_key'].split()[:2]) + '\n')
        config = tmp / 'config'
        config.write_text(f'''Include "{ACCESS / 'ssh_config'}"
Host proof
    HostName {entry['address']}
    User dev
    IdentityFile "{Path.home() / '.ssh/id_ed25519'}"
    IdentitiesOnly yes
    BatchMode yes
    StrictHostKeyChecking yes
    UserKnownHostsFile "{known}"
    HostKeyAlias {alias}
    ConnectTimeout 10
    ProxyCommand /usr/bin/ssh -F "{ACCESS / 'ssh_config'}" -W %h:%p hclab-host
''')
        def ssh(command):
            return subprocess.run(['/usr/bin/ssh', '-F', str(config), 'proof', command],
                capture_output=True, text=True, check=True, timeout=25).stdout.strip()
        assert ssh('id -un') == 'dev'
        source, returned = tmp / 'source.bin', tmp / 'returned.bin'
        payload = os.urandom(256 * 1024)
        source.write_bytes(payload)
        batch = f'put {source} /home/dev/sftp-proof\nget /home/dev/sftp-proof {returned}\nrm /home/dev/sftp-proof\n'
        subprocess.run(['/usr/bin/sftp', '-F', str(config), '-b', '-', 'proof'],
            input=batch, capture_output=True, text=True, check=True, timeout=40)
        assert returned.read_bytes() == payload
        entry['mac_ssh_as_dev'] = True
        entry['sftp_bytes'] = len(payload)
        entry['sftp_sha256'] = hashlib.sha256(payload).hexdigest()
        entry['restart'] = json.loads(script('provisioning_guest.py', 'restart', number, timeout=300))
        assert ssh('id -un') == 'dev'
        entry['mac_ssh_after_restart'] = True
    print(f'Guest {number}: fresh bootstrap, network fence, SSH/SFTP, restart passed', flush=True)
    entry['cleanup'] = json.loads(script('provisioning_guest.py', 'remove', number))
    save('provisioning.json', report)
assert len({g['uuid'] for g in report['guests']}) == 3
assert len({' '.join(g['host_key'].split()[:2]) for g in report['guests']}) == 3
report.update(passed=True, distinct_identities=True, completed_at=time.time())
save('provisioning.json', report)
print('Fresh provisioning proof passed for all three guests', flush=True)
