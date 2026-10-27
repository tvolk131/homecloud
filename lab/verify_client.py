#!/usr/bin/env python3
"""Verify the Mac's SSH/SFTP path and rejection of a mismatched guest key."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent.parent
access = root / '.host-access'
config = access / 'ssh_config'
report = json.loads((root / 'lab/evidence/verification.json').read_text())
connected = subprocess.run(['/usr/bin/ssh', '-F', str(config), '-o', 'BatchMode=yes',
    'hclab-vm01', 'id -un'], capture_output=True, text=True, check=True, timeout=20)
assert connected.stdout.strip() == 'dev', connected.stdout

transfer = access / 'transfer-smoke'
transfer.mkdir(exist_ok=True)
source = transfer / 'source.bin'
returned = transfer / 'returned.bin'
payload = os.urandom(256 * 1024)
source.write_bytes(payload)
batch = f'put {source} /home/dev/.home-cloud-sftp-smoke\nget /home/dev/.home-cloud-sftp-smoke {returned}\nrm /home/dev/.home-cloud-sftp-smoke\n'
subprocess.run(['/usr/bin/sftp', '-F', str(config), '-b', '-', 'hclab-vm01'],
    input=batch, capture_output=True, text=True, check=True, timeout=30)
assert returned.read_bytes() == payload, 'SFTP roundtrip integrity failed'

alias = 'homecloud-' + report['ssh']['hclab-vm01']['vm_uuid']
wrong_key = ' '.join(report['ssh']['hclab-vm02']['host_public_key'].split()[:2])
wrong_hosts = transfer / 'known_hosts-mismatch'
wrong_hosts.write_text(f'{alias} {wrong_key}\n')
try:
    rejected = subprocess.run(['/usr/bin/ssh', '-F', str(config), '-o', 'BatchMode=yes',
        '-o', f'UserKnownHostsFile={wrong_hosts}', 'hclab-vm01', 'true'],
        capture_output=True, text=True, timeout=20)
    assert rejected.returncode == 255 and 'REMOTE HOST IDENTIFICATION HAS CHANGED' in rejected.stderr, rejected.stderr
finally:
    wrong_hosts.unlink(missing_ok=True)
source.unlink()
returned.unlink()
result = {'ssh_user': connected.stdout.strip(), 'sftp_roundtrip_bytes': len(payload),
          'sftp_sha256': hashlib.sha256(payload).hexdigest(),
          'changed_guest_host_key_rejected': True,
          'transport': 'OpenSSH through the host; Iroh not implemented yet'}
(root / 'lab/evidence/client-verification.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
