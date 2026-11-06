"""Seed only the manifest-owned PVE VM with stopped guests and read-only tokens."""

import hashlib
import json
from pathlib import Path
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


def run(*arguments):
    result = subprocess.run(arguments, text=True, capture_output=True, timeout=120)
    if result.returncode:
        raise RuntimeError('Guest seed command failed: ' + ' '.join(arguments[:3]) + ': ' + result.stderr[-1200:])
    return result.stdout.strip()


def settled_inventory(resources):
    by_id = {entry['vmid']: entry for entry in resources}
    return set(by_id) == {7101, 7199} and by_id[7199].get('template') == 1 and not by_id[7101].get('template') and all(
        entry.get('maxcpu') == 2 and entry.get('maxmem') == 512 * 1024**2
        and entry.get('maxdisk') == 1024**3 for entry in resources)


def main():
    action, token = sys.argv[1:3]
    directory = Path('/root/homecloud-proxmox-spike')
    if action != 'seed' or (directory / 'run-id').read_text() != token:
        raise RuntimeError('Foreign guest or operation')
    node = run('hostname', '-s')
    if node != 'hc-pve-' + token[:10]:
        raise RuntimeError('Unexpected PVE node')
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        services = subprocess.run(['systemctl', 'is-active', 'pveproxy', 'pvedaemon', 'pvestatd'],
                                  text=True, capture_output=True, timeout=15)
        if services.stdout.strip().splitlines() == ['active', 'active', 'active']:
            break
        time.sleep(3)
    else:
        raise RuntimeError('PVE services did not become ready')
    audit = run('dpkg', '--audit')
    if audit:
        raise RuntimeError('Disposable PVE package audit is not clean: ' + audit)
    expires = str(int(time.time()) + 14400)
    username = 'hc-cockpit@pve'
    run('pveum', 'user', 'add', username, '--comment', 'Homecloud disposable ' + token, '--expire', expires)
    run('pveum', 'acl', 'modify', '/', '--users', username, '--roles', 'PVEAuditor', '--propagate', '1')
    credentials = {}
    for name in ('reader', 'noaccess'):
        credentials[name] = json.loads(run('pveum', 'user', 'token', 'add', username, name,
                                            '--privsep', '1', '--expire', expires, '--output-format', 'json'))
    run('pveum', 'acl', 'modify', '/', '--tokens', username + '!reader', '--roles', 'PVEAuditor', '--propagate', '1')
    for name, credential in credentials.items():
        filename = directory / (name + '-token.json')
        filename.write_text(json.dumps(credential))
        filename.chmod(0o600)
    run('pvesm', 'add', 'dir', 'hc-cockpit', '--path', '/var/lib/hc-cockpit-' + token,
        '--content', 'images,rootdir,iso')
    for vmid, name in (('7101', 'hc-live-stopped'), ('7199', 'hc-live-template')):
        run('qm', 'create', vmid, '--name', name, '--memory', '512', '--cores', '2',
            '--scsi0', 'hc-cockpit:1', '--description', 'Owned disposable run ' + token,
            '--tags', 'homecloud-live-validation', '--onboot', '0')
    run('qm', 'template', '7199')
    context = ssl.create_default_context(cafile='/etc/pve/pve-root-ca.pem')
    context.check_hostname = False
    credential = credentials['reader']
    read_request = urllib.request.Request('https://127.0.0.1:8006/api2/extjs/cluster/resources?type=vm',
        headers={'Authorization': 'PVEAPIToken=' + credential['full-tokenid'] + '=' + credential['value']})
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        configuration_template = 'template: 1' in run('qm', 'config', '7199')
        resources = json.loads(run('pvesh', 'get', '/cluster/resources', '--type', 'vm', '--output-format', 'json'))
        with urllib.request.urlopen(read_request, context=context, timeout=20) as response:
            token_resources = json.load(response)['data']
        if configuration_template and all(settled_inventory(snapshot) for snapshot in (resources, token_resources)):
            break
        time.sleep(3)
    else:
        raise RuntimeError('Seeded VM capacity/template state did not settle in native PVE inventory')
    certificate = Path('/etc/pve/local/pve-ssl.pem').read_text()
    fingerprint = hashlib.sha256(ssl.PEM_cert_to_DER_cert(certificate)).hexdigest()
    (directory / 'tls-fingerprint').write_text(fingerprint)
    request = urllib.request.Request(
        'https://127.0.0.1:8006/api2/json/nodes/' + node + '/qemu/7101/config',
        method='POST', data=urllib.parse.urlencode({'name': 'must-not-apply'}).encode(),
        headers={'Authorization': 'PVEAPIToken=' + credential['full-tokenid'] + '=' + credential['value']},
    )
    try:
        urllib.request.urlopen(request, context=context, timeout=20)
    except urllib.error.HTTPError as error:
        denied = error.code
    else:
        raise RuntimeError('Read-only token unexpectedly permitted VM mutation')
    if denied != 403 or 'name: hc-live-stopped' not in run('qm', 'config', '7101'):
        raise RuntimeError('Mutation-denial assertion failed')
    permissions = json.loads(run('pveum', 'user', 'token', 'permissions', username, 'reader', '--path', '/', '--output-format', 'json'))
    print(json.dumps({
        'node': node, 'pve_version': run('pveversion'), 'kernel': run('uname', '-r'),
        'services': services.stdout.strip().splitlines(), 'package_audit': audit,
        'native_seed_resources': resources,
        'readonly_seed_resources': token_resources,
        'native_config_template': configuration_template,
        'fingerprint_sha256': fingerprint,
        'token_mode': 'private files; PVEAuditor on user AND privsep=1 token; four-hour expiry',
        'effective_permissions': permissions, 'write_denied_http': denied,
        'seeded_vmids': [7101, 7199], 'existing_guest_touched': False,
    }))


if __name__ == '__main__':
    main()
