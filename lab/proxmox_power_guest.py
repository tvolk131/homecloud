"""Least-privilege credentials inside the existing owned cockpit fixture only."""

import json
from pathlib import Path
import sys
import time

import proxmox_cockpit_guest as cockpit


def main():
    action, token = sys.argv[1:]
    if action != 'power-seed' or len(token) != 32 or any(character not in '0123456789abcdef' for character in token):
        raise RuntimeError('Invalid power fixture identity')
    directory = Path('/root/homecloud-proxmox-spike')
    if (directory / 'run-id').read_text() != token:
        raise RuntimeError('Foreign disposable guest')
    node = cockpit.run('hostname', '-s')
    if node != 'hc-pve-' + token[:10]:
        raise RuntimeError('Unexpected disposable node')
    for vmid in ('7101', '7199'):
        config = cockpit.run('qm', 'config', vmid)
        if 'description: Owned disposable run ' + token not in config:
            raise RuntimeError('Foreign nested VM')
    username = 'hc-power@pve'
    role = 'HCPower' + token[:10]
    expires = str(int(time.time()) + 14400)
    cockpit.run('pveum', 'user', 'add', username, '--expire', expires,
                '--comment', 'Owned Homecloud power validation ' + token)
    cockpit.run('pveum', 'role', 'add', role, '--privs', 'VM.Audit VM.PowerMgmt')
    credential = json.loads(cockpit.run('pveum', 'user', 'token', 'add', username, 'power',
                                      '--privsep', '1', '--expire', expires, '--output-format', 'json'))
    for principal, identifier in (('--users', username), ('--tokens', credential['full-tokenid'])):
        cockpit.run('pveum', 'acl', 'modify', '/', principal, identifier,
                    '--roles', 'PVEAuditor', '--propagate', '1')
        cockpit.run('pveum', 'acl', 'modify', '/vms/7101', principal, identifier,
                    '--roles', role, '--propagate', '0')
    filename = directory / 'power-token.json'
    filename.write_text(json.dumps(credential))
    filename.chmod(0o600)
    permissions = json.loads(cockpit.run('pveum', 'user', 'token', 'permissions', username, 'power',
                                       '--path', '/vms/7101', '--output-format', 'json'))
    print(json.dumps({'node': node, 'owned_vmids': [7101, 7199], 'power_scope': '/vms/7101',
                      'power_privileges': ['VM.Audit', 'VM.PowerMgmt'],
                      'inventory_role': 'PVEAuditor', 'privsep': True,
                      'effective_permissions': permissions, 'expires_after_seconds': 14400,
                      'bootable_os_installed': False, 'original_guests_touched': False}))


if __name__ == '__main__':
    main()
