"""Fixed operations inside the manifest-owned disposable Proxmox worker."""

import base64
import hashlib
import json
import os
from pathlib import Path
import shlex
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

ACTION, TOKEN = sys.argv[1:3]
DIRECTORY = Path('/root/homecloud-proxmox-spike')
DIRECTORY.mkdir(exist_ok=True, mode=0o700)
MARKER = DIRECTORY / 'run-id'
if MARKER.exists() and MARKER.read_text() != TOKEN:
    raise RuntimeError('Foreign guest run')
MARKER.write_text(TOKEN)
MARKER.chmod(0o600)


def run(*arguments, timeout=120, check=True, input=None):
    result = subprocess.run(arguments, input=input, text=True, capture_output=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(' '.join(arguments[:3]) + ': ' + (result.stderr or result.stdout)[-2500:])
    return result


def install():
    if 'Debian GNU/Linux 13' not in Path('/etc/os-release').read_text():
        raise RuntimeError('Expected disposable Debian 13 guest')
    if not Path('/dev/kvm').exists():
        raise RuntimeError('Nested KVM unavailable')
    root = Path(run('findmnt', '-n', '-o', 'SOURCE', '/').stdout.strip()).resolve()
    parent = run('lsblk', '-n', '-o', 'PKNAME', str(root)).stdout.strip()
    partition_path = Path('/sys/class/block') / root.name / 'partition'
    if not partition_path.exists() or run('findmnt', '-n', '-o', 'FSTYPE', '/').stdout.strip() != 'ext4':
        raise RuntimeError('Unexpected worker root layout')
    if int(run('blockdev', '--getsize64', '/dev/' + parent).stdout) != 32 * 1024**3:
        raise RuntimeError('Unexpected worker root disk size')
    grown = run('growpart', '/dev/' + parent, partition_path.read_text().strip(), check=False)
    if grown.returncode and 'NOCHANGE' not in grown.stdout:
        raise RuntimeError('Unable to expand worker root: ' + grown.stderr)
    run('resize2fs', str(root))
    interfaces = [path.name for path in Path('/sys/class/net').iterdir() if path.name != 'lo']
    if len(interfaces) != 1:
        raise RuntimeError('Unexpected worker uplinks')
    interface = interfaces[0]
    Path('/etc/systemd/network/10-homecloud-spike.network').write_text(
        '[Match]\nName=' + interface + '\n[Network]\nAddress=172.31.246.2/24\n'
        'Gateway=172.31.246.1\nDNS=172.31.246.1\n')
    run('systemctl', 'restart', 'systemd-networkd')
    Path('/etc/network').mkdir(exist_ok=True)
    Path('/etc/network/interfaces.d').mkdir(exist_ok=True)
    Path('/etc/network/interfaces').write_text(
        'auto lo\niface lo inet loopback\n\nauto ' + interface + '\niface ' + interface +
        ' inet static\n    address 172.31.246.2/24\n    gateway 172.31.246.1\n'
        '\nsource /etc/network/interfaces.d/*\n')
    resolver = Path('/etc/resolv.conf')
    if resolver.is_symlink():
        resolver.unlink()
    resolver.write_text('nameserver 172.31.246.1\n')
    hostname = 'hc-pve-' + TOKEN[:10]
    Path('/etc/hostname').write_text(hostname + '\n')
    run('hostname', hostname)
    hosts = [line for line in Path('/etc/hosts').read_text().splitlines()
             if hostname not in line and not line.startswith('127.0.1.1')]
    Path('/etc/hosts').write_text('\n'.join(hosts) + '\n172.31.246.2 ' + hostname + '\n')
    key = urllib.request.urlopen('https://enterprise.proxmox.com/debian/proxmox-archive-keyring-trixie.gpg', timeout=60).read()
    Path('/usr/share/keyrings/proxmox-archive-keyring.gpg').write_bytes(key)
    Path('/etc/apt/sources.list.d/proxmox.sources').write_text(
        'Types: deb\nURIs: http://download.proxmox.com/debian/pve\nSuites: trixie\n'
        'Components: pve-no-subscription\nSigned-By: /usr/share/keyrings/proxmox-archive-keyring.gpg\n')
    Path('/etc/modprobe.d/homecloud-spike-zfs.conf').write_text('options zfs zfs_arc_max=536870912\n')
    environment = dict(os.environ, DEBIAN_FRONTEND='noninteractive')
    log = DIRECTORY / 'install.log'
    with log.open('a') as output:
        subprocess.run(['dpkg', '--configure', '-a'], stdout=output, stderr=subprocess.STDOUT,
                       env=environment, timeout=1500)
        for arguments in (['apt-get', 'update'], ['apt-get', '-f', 'install', '-y'],
                          ['apt-get', 'install', '-y', 'proxmox-ve', 'postfix', 'open-iscsi', 'chrony', 'curl', 'qemu-guest-agent', 'zfsutils-linux']):
            completed = subprocess.run(arguments, stdout=output, stderr=subprocess.STDOUT, env=environment, timeout=1500)
            if completed.returncode:
                raise RuntimeError('Package installation failed; inspect ' + str(log))
    run('systemctl', 'disable', 'systemd-networkd', 'systemd-resolved')
    return {'installed': True, 'versions': run('pveversion', '-v').stdout.strip(), 'reboot_required': True}


def wait_services():
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        services = run('systemctl', 'is-active', 'pveproxy', 'pvedaemon', 'pvestatd', check=False)
        if services.returncode == 0 and services.stdout.splitlines() == ['active', 'active', 'active']:
            return services.stdout.strip()
        time.sleep(3)
    raise RuntimeError('Proxmox services did not become ready: ' + services.stdout)


def probe():
    services = wait_services()
    return {'kernel': run('uname', '-r').stdout.strip(), 'versions': run('pveversion', '-v').stdout.strip(),
            'services': services,
            'package_audit': run('dpkg', '--audit').stdout.strip(),
            'kvm': Path('/dev/kvm').exists(), 'disks': json.loads(run('lsblk', '--json', '-o', 'NAME,SIZE,TYPE,SERIAL,MOUNTPOINTS').stdout)}


def api(method, path, parameters=None, credential=None):
    credential_path = DIRECTORY / 'api-token.json'
    if credential is None:
        if not credential_path.exists():
            value = json.loads(run('pvesh', 'create', '/access/users/root@pam/token/hc-spike',
                                   '--privsep', '0', '--expire', str(int(time.time()) + 43200),
                                   '--output-format', 'json').stdout)
            credential_path.write_text(json.dumps(value))
            credential_path.chmod(0o600)
        credential = json.loads(credential_path.read_text())
    headers = {'Authorization': 'PVEAPIToken=' + credential['full-tokenid'] + '=' + credential['value']}
    payload = None
    if parameters is not None:
        if method in ('GET', 'DELETE'):
            path += '?' + urllib.parse.urlencode(parameters)
        else:
            headers['Content-Type'] = 'application/json'
            payload = json.dumps(parameters).encode()
    request = urllib.request.Request('https://127.0.0.1:8006/api2/json' + path,
                                     data=payload, headers=headers, method=method)
    with urllib.request.urlopen(request, context=ssl._create_unverified_context(), timeout=90) as response:
        return json.load(response)['data']


def node():
    return run('hostname', '-s').stdout.strip()


def task(upid, timeout=300):
    if not isinstance(upid, str) or not upid.startswith('UPID:'):
        return upid
    deadline = time.monotonic() + timeout
    path = '/nodes/' + node() + '/tasks/' + urllib.parse.quote(upid, safe='')
    while time.monotonic() < deadline:
        status = api('GET', path + '/status')
        if status['status'] == 'stopped':
            if status.get('exitstatus') != 'OK':
                raise RuntimeError('Proxmox task failed: ' + str(status))
            return status
        time.sleep(2)
    raise TimeoutError('Proxmox task did not finish')


def ensure_pool():
    run('modprobe', 'zfs')
    if run('zpool', 'list', 'hcspike', check=False).returncode == 0:
        if run('zfs', 'get', '-H', '-o', 'value', 'org.homecloud:spike', 'hcspike').stdout.strip() != TOKEN:
            raise RuntimeError('Foreign inner ZFS pool')
        return
    devices = json.loads(run('lsblk', '--bytes', '--json', '-o', 'PATH,SIZE,TYPE,SERIAL,MOUNTPOINTS').stdout)['blockdevices']
    candidates = [device for device in devices if device['type'] == 'disk' and device['size'] == 64 * 1024**3
                  and 'scratch' in (device.get('serial') or '') and not device.get('children')
                  and not any(device.get('mountpoints') or [])]
    if len(candidates) != 1:
        raise RuntimeError('Scratch disk identity is ambiguous: ' + str(devices))
    device = candidates[0]
    intent = DIRECTORY / 'pool-intent.json'
    if intent.exists() and json.loads(intent.read_text()) != device:
        raise RuntimeError('Scratch disk changed')
    intent.write_text(json.dumps(device))
    if run('blkid', '-p', device['path'], check=False).returncode != 2:
        raise RuntimeError('Scratch disk has an unexpected signature')
    run('zpool', 'create', '-o', 'ashift=12', '-O', 'compression=lz4', '-O', 'mountpoint=/hcspike', 'hcspike', device['path'])
    run('zfs', 'set', 'org.homecloud:spike=' + TOKEN, 'hcspike')


def ensure_storages():
    ensure_pool()
    existing = {entry['storage'] for entry in api('GET', '/storage')}
    for tenant, size in (('base', 12), ('alice', 16), ('bob', 16)):
        dataset = 'hcspike/' + tenant
        if run('zfs', 'list', dataset, check=False).returncode:
            run('zfs', 'create', '-o', 'quota=' + str(size) + 'G', '-o', 'reservation=' + str(size) + 'G', dataset)
        storage = 'hc-' + tenant
        if storage not in existing:
            api('POST', '/storage', {'storage': storage, 'type': 'zfspool', 'pool': dataset,
                                    'content': 'images,rootdir', 'sparse': 0})


def properties(dataset):
    result = run('zfs', 'get', '-Hp', '-o', 'property,value',
                 'used,available,quota,reservation,usedbysnapshots,usedbyrefreservation', dataset)
    return {line.split('\t')[0]: int(line.split('\t')[1]) for line in result.stdout.splitlines()}


def bounded_write(path, target):
    block = os.urandom(1024**2)
    completed = 0
    failure = None
    try:
        with Path(path).open('wb', buffering=0) as output:
            while completed < target:
                completed += output.write(block[:min(len(block), target - completed)])
            os.fsync(output.fileno())
    except OSError as error:
        failure = {'errno': error.errno, 'message': str(error)}
    run('zpool', 'sync', 'hcspike')
    return {'written_bytes': completed, 'target_bytes': target, 'error': failure}


def storage_test():
    ensure_storages()
    alice = 'hcspike/alice'
    probe_dataset = alice + '/cap-probe'
    if run('zfs', 'list', probe_dataset, check=False).returncode:
        run('zfs', 'create', '-o', 'compression=off', probe_dataset)
    run('zfs', 'set', 'compression=off', 'hcspike/bob')
    before = properties(alice)
    cap = before['used'] + 96 * 1024**2
    run('zfs', 'set', 'reservation=' + str(cap), alice)
    run('zfs', 'set', 'quota=' + str(cap), alice)
    try:
        blocked = bounded_write('/hcspike/alice/cap-probe/fill.bin', 192 * 1024**2)
        if not blocked['error'] or blocked['error']['errno'] not in (28, 122):
            raise RuntimeError('Tenant quota failed to reject bounded write: ' + str(blocked))
        bob_write = bounded_write('/hcspike/bob/independent.bin', 32 * 1024**2)
        if bob_write['error']:
            raise RuntimeError('Other tenant was affected: ' + str(bob_write))
        capped = properties(alice)
    finally:
        Path('/hcspike/alice/cap-probe/fill.bin').unlink(missing_ok=True)
        run('zfs', 'set', 'quota=' + str(before['quota']), alice)
        run('zfs', 'set', 'reservation=' + str(before['reservation']), alice)
    snapshot_dataset = alice + '/snapshot-probe'
    if run('zfs', 'list', snapshot_dataset, check=False).returncode:
        run('zfs', 'create', '-o', 'compression=off', '-o', 'quota=64M', snapshot_dataset)
    snapshots = []
    rejected = None
    for index in range(8):
        write = bounded_write('/hcspike/alice/snapshot-probe/data.bin', 16 * 1024**2)
        if write['error']:
            rejected = {'operation': 'write', **write}
            break
        snapshot_name = snapshot_dataset + '@round-' + str(index)
        result = run('zfs', 'snapshot', snapshot_name, check=False)
        if result.returncode:
            rejected = {'operation': 'snapshot', 'message': result.stderr.strip()}
            break
        snapshots.append(snapshot_name)
    if rejected is None:
        raise RuntimeError('Snapshot churn did not hit its bounded quota')
    snapshot_properties = properties(snapshot_dataset)
    if snapshot_properties['usedbysnapshots'] == 0:
        raise RuntimeError('Snapshot storage was not accounted')
    return {'quota_write_rejected': blocked, 'alice_at_cap': capped, 'bob_write_succeeded': bob_write,
            'snapshot_churn_rejected': rejected, 'snapshot_accounting': snapshot_properties,
            'snapshots_created': len(snapshots), 'storages': api('GET', '/storage'),
            'enforcement': 'Host ZFS; Homecloud controller remains stopped'}


def ensure_bridge():
    if run('ip', 'link', 'show', 'vmbr0', check=False).returncode:
        run('ip', 'link', 'add', 'vmbr0', 'type', 'bridge')
        run('ip', 'address', 'add', '10.253.211.1/24', 'dev', 'vmbr0')
    run('ip', 'link', 'set', 'vmbr0', 'up')
    run('sysctl', '-w', 'net.ipv4.ip_forward=1')
    Path('/etc/network/interfaces.d/homecloud-spike').write_text(
        'auto vmbr0\niface vmbr0 inet static\n    address 10.253.211.1/24\n'
        '    bridge-ports none\n    bridge-stp off\n    bridge-fd 0\n')
    rules = ('table ip hc_spike {\n chain postrouting {\n'
             '  type nat hook postrouting priority 100; policy accept;\n'
             '  ip saddr 10.253.211.0/24 oifname != "vmbr0" masquerade\n }\n}\n')
    if run('nft', 'list', 'table', 'ip', 'hc_spike', check=False).returncode:
        run('nft', '-f', '-', input=rules)


def guest_command(vmid, command, timeout=180):
    endpoint = '/nodes/' + node() + '/qemu/' + str(vmid) + '/agent'
    process = api('POST', endpoint + '/exec', {'command': ['/bin/sh', '-c', command]})
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = api('GET', endpoint + '/exec-status', {'pid': process['pid']})
        if status.get('exited'):
            if status.get('exitcode') != 0:
                raise RuntimeError('Guest command failed: ' + str(status)[-2000:])
            return status.get('out-data', '').strip()
        time.sleep(2)
    raise TimeoutError('Guest command did not complete')


def wait_guest(vmid):
    deadline = time.monotonic() + 360
    last_error = None
    while time.monotonic() < deadline:
        try:
            guest_command(vmid, 'true', timeout=15)
            return
        except (urllib.error.HTTPError, RuntimeError, TimeoutError) as error:
            last_error = str(error)
            time.sleep(4)
    raise TimeoutError('Guest agent unavailable: ' + str(last_error))


def vm_path(vmid):
    return '/nodes/' + node() + '/qemu/' + str(vmid)


def ensure_template():
    ensure_storages()
    ensure_bridge()
    image = DIRECTORY / 'debian-13-genericcloud-amd64.qcow2'
    image_url = 'https://cloud.debian.org/images/cloud/trixie/latest/'
    if not image.exists():
        with urllib.request.urlopen(image_url + image.name, timeout=90) as response, image.open('wb') as output:
            while True:
                block = response.read(1024**2)
                if not block:
                    break
                output.write(block)
    checksum_text = urllib.request.urlopen(image_url + 'SHA512SUMS', timeout=60).read().decode()
    checksums = {Path(line.split()[-1].lstrip('*')).name: line.split()[0] for line in checksum_text.splitlines() if line.strip()}
    digest = hashlib.sha512(image.read_bytes()).hexdigest()
    if checksums.get(image.name) != digest:
        raise RuntimeError('Official Debian image checksum mismatch')
    key = DIRECTORY / 'guest-key'
    if not key.exists():
        run('ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(key))
    public_key = key.with_suffix('.pub').read_text().strip()
    snippets = Path('/var/lib/vz/snippets')
    snippets.mkdir(exist_ok=True)
    userdata = '#cloud-config\nusers:\n  - name: dev\n    groups: [sudo]\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    shell: /bin/bash\n    ssh_authorized_keys:\n      - ' + public_key + '\nssh_pwauth: false\npackage_update: true\npackages: [qemu-guest-agent, curl, ca-certificates, python3, dbus-user-session, libatomic1, git]\nruncmd:\n  - [systemctl, enable, --now, qemu-guest-agent]\n'
    (snippets / 'homecloud-spike.yaml').write_text(userdata)
    api('PUT', '/storage/local', {'content': 'iso,vztmpl,backup,snippets'})
    if 'hc-team' not in {entry['poolid'] for entry in api('GET', '/pools')}:
        api('POST', '/pools', {'poolid': 'hc-team', 'comment': TOKEN})
    existing = {entry['vmid'] for entry in api('GET', '/nodes/' + node() + '/qemu')}
    if 9000 not in existing:
        api('POST', '/nodes/' + node() + '/qemu', {
            'vmid': 9000, 'name': 'hc-spike-base', 'description': TOKEN, 'memory': 1536, 'cores': 1,
            'net0': 'virtio,bridge=vmbr0', 'scsihw': 'virtio-scsi-single', 'agent': 'enabled=1',
            'serial0': 'socket', 'vga': 'serial0', 'pool': 'hc-team',
        })
        run('qm', 'importdisk', '9000', str(image), 'hc-base', timeout=180)
        api('PUT', vm_path(9000) + '/config', {
            'scsi0': 'hc-base:vm-9000-disk-0', 'ide2': 'hc-base:cloudinit', 'boot': 'order=scsi0',
            'cicustom': 'user=local:snippets/homecloud-spike.yaml', 'nameserver': '1.1.1.1',
        })
        task(api('PUT', vm_path(9000) + '/resize', {'disk': 'scsi0', 'size': '8G'}))
        task(api('POST', vm_path(9000) + '/template'))
    return {'image_sha512': digest, 'template_vmid': 9000}


def expect_denied(method, path, parameters=None, credential=None):
    try:
        result = api(method, path, parameters, credential=credential)
        task(result)
    except urllib.error.HTTPError as error:
        return {'rejected': True, 'http_status': error.code, 'message': error.read().decode()[-1200:]}
    except RuntimeError as error:
        return {'rejected': True, 'message': str(error)[-1200:]}
    raise RuntimeError('Operation unexpectedly succeeded: ' + path)


def templates_test():
    result = ensure_template()
    existing = {entry['vmid'] for entry in api('GET', '/nodes/' + node() + '/qemu')}
    configurations = {}
    for vmid, tenant, address in ((9001, 'alice', 11), (9002, 'bob', 12)):
        if vmid not in existing:
            task(api('POST', vm_path(9000) + '/clone', {
                'newid': vmid, 'name': 'hc-spike-' + tenant, 'full': 1,
                'storage': 'hc-' + tenant, 'pool': 'hc-team', 'description': TOKEN,
            }))
            api('PUT', vm_path(vmid) + '/config', {'ipconfig0': 'ip=10.253.211.' + str(address) + '/24,gw=10.253.211.1'})
        if api('GET', vm_path(vmid) + '/status/current')['status'] != 'running':
            task(api('POST', vm_path(vmid) + '/status/start'))
        wait_guest(vmid)
        guest_command(vmid, 'cloud-init status --wait >/dev/null; printf %s ' + TOKEN + ' >/home/dev/spike-proof')
        configurations[tenant] = api('GET', vm_path(vmid) + '/config')
        if not configurations[tenant]['scsi0'].startswith('hc-' + tenant + ':'):
            raise RuntimeError('Clone escaped tenant storage')
    result['oversized_resize'] = expect_denied('PUT', vm_path(9001) + '/resize', {'disk': 'scsi0', 'size': '+32G'})
    if not any(word in str(result['oversized_resize']).lower() for word in ('space', 'quota', 'reservation')):
        raise RuntimeError('Resize failed for a reason unrelated to disk enforcement')
    result['alice_after_rejection'] = guest_command(9001, 'printf safe >/home/dev/still-writable; sync; cat /home/dev/spike-proof')
    result['bob_after_rejection'] = guest_command(9002, 'printf safe >/home/dev/still-writable; sync; cat /home/dev/spike-proof')
    if result['alice_after_rejection'] != TOKEN or result['bob_after_rejection'] != TOKEN:
        raise RuntimeError('Guest became unhealthy after rejected resize')
    users = {entry['userid'] for entry in api('GET', '/access/users')}
    if 'hc-alice@pve' not in users:
        api('POST', '/access/users', {'userid': 'hc-alice@pve', 'comment': TOKEN})
        api('PUT', '/access/acl', {'path': '/vms/9001', 'users': 'hc-alice@pve', 'roles': 'PVEVMUser'})
    credential_path = DIRECTORY / 'alice-token.json'
    if not credential_path.exists():
        credential = api('POST', '/access/users/hc-alice@pve/token/hc-spike', {'privsep': 0, 'expire': int(time.time()) + 43200})
        credential_path.write_text(json.dumps(credential))
        credential_path.chmod(0o600)
    credential = json.loads(credential_path.read_text())
    result['alice_rbac'] = {
        'own_vm_status': api('GET', vm_path(9001) + '/status/current', credential=credential)['status'],
        'other_vm': expect_denied('GET', vm_path(9002) + '/config', credential=credential),
        'host_storage_admin': expect_denied('POST', '/storage', {'storage': 'forbidden', 'type': 'dir', 'path': '/tmp/forbidden'}, credential=credential),
    }
    if any(result['alice_rbac'][key].get('http_status') != 403 for key in ('other_vm', 'host_storage_admin')):
        raise RuntimeError('RBAC denial was not an authorization failure')
    result['configurations'] = {tenant: {key: value for key, value in configuration.items() if key in ('name', 'scsi0', 'ide2', 'memory', 'cores')}
                                for tenant, configuration in configurations.items()}
    result['usage'] = {tenant: {'storage': properties('hcspike/' + tenant),
                               'vm': {key: value for key, value in api('GET', vm_path(vmid) + '/status/current').items()
                                      if key in ('status', 'cpu', 'mem', 'maxmem', 'maxdisk', 'disk', 'diskread', 'diskwrite', 'cpus')}}
                       for tenant, vmid in (('alice', 9001), ('bob', 9002))}
    for tenant in ('alice', 'bob'):
        volume = configurations[tenant]['scsi0'].split(':', 1)[1].split(',', 1)[0]
        raw = run('zfs', 'get', '-Hp', '-o', 'property,value', 'volsize,refreservation,used', 'hcspike/' + tenant + '/' + volume)
        backing = {line.split('\t')[0]: int(line.split('\t')[1]) for line in raw.stdout.splitlines()}
        if backing['refreservation'] < backing['volsize']:
            raise RuntimeError('Guest volume backing was not reserved')
        result['usage'][tenant]['root_volume_backing'] = backing
    return result


def t3_status(vmid):
    command = """uid=$(id -u dev)
runuser -u dev -- env HOME=/home/dev XDG_RUNTIME_DIR=/run/user/$uid DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$uid/bus systemctl --user is-active t3code.service
runuser -u dev -- /home/dev/.local/bin/t3 --version
curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:3773/
printf '\\n'
cat /proc/sys/kernel/random/boot_id
wc -l </home/dev/spike-heartbeat.log
"""
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        status = guest_command(vmid, command).splitlines()
        if len(status) >= 5 and status[0] == 'active' and status[-3] in ('200', '301', '302', '401', '403'):
            if 't3 v0.1.2' not in status:
                raise RuntimeError('Unexpected T3 version')
            return status
        time.sleep(3)
    raise RuntimeError('T3 service did not become ready: ' + str(status))


def t3_test():
    vmid = 9001
    wait_guest(vmid)
    transition_path = DIRECTORY / 'cpu-transition.json'
    configuration = api('GET', vm_path(vmid) + '/config')
    if configuration.get('cpu') != 'host':
        before_flags = guest_command(vmid, "grep -m1 '^flags' /proc/cpuinfo")
        transition_path.write_text(json.dumps({'original_model': configuration.get('cpu', 'API default'),
                                              'original_flags': before_flags, 'selected_model': 'host'}))
        api('PUT', vm_path(vmid) + '/config', {'cpu': 'host'})
        task(api('POST', vm_path(vmid) + '/status/shutdown', {'timeout': 60}))
        task(api('POST', vm_path(vmid) + '/status/start'))
        wait_guest(vmid)
    payload = base64.b64encode(json.dumps(ASSETS).encode()).decode()
    installer = """import base64,json,pathlib,subprocess
sources=json.loads(base64.b64decode('PAYLOAD'))
directory=pathlib.Path('/usr/local/libexec/homecloud')
directory.mkdir(parents=True,exist_ok=True)
for name,source in sources.items():
 path=directory/name
 path.write_text(source)
 path.chmod(0o644)
uid=subprocess.check_output(['id','-u','dev'],text=True).strip()
subprocess.run(['loginctl','enable-linger','dev'],check=True)
subprocess.run(['systemctl','start','user@'+uid+'.service'],check=True)
status=pathlib.Path('/home/dev/spike-install.exit')
if not status.exists() or status.read_text().strip()!='0':
 status.unlink(missing_ok=True)
 subprocess.run(['systemctl','reset-failed','homecloud-spike-t3-install.service'],check=False,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 command='/bin/sh /usr/local/libexec/homecloud/setup-t3.sh 0.1.2 >/home/dev/spike-install.log 2>&1; code=$?; printf %s "$code" >/home/dev/spike-install.exit; exit "$code"'
 subprocess.run(['systemd-run','--quiet','--unit=homecloud-spike-t3-install','--uid=dev',
  '--setenv=HOME=/home/dev','--setenv=PATH=/home/dev/.local/bin:/usr/local/bin:/usr/bin:/bin',
  '--setenv=XDG_RUNTIME_DIR=/run/user/'+uid,'--setenv=DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/'+uid+'/bus',
  '/bin/sh','-c',command],check=True)
""".replace('PAYLOAD', payload)
    guest_command(vmid, "python3 - <<'PY'\n" + installer + '\nPY')
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline:
        status = guest_command(vmid, 'cat /home/dev/spike-install.exit 2>/dev/null || printf pending')
        if status == '0':
            break
        if status != 'pending':
            raise RuntimeError('T3 install failed; inspect private guest /home/dev/spike-install.log')
        time.sleep(5)
    else:
        raise TimeoutError('T3 installation did not finish')
    heartbeat = """from pathlib import Path
import time
while True:
 with Path('/home/dev/spike-heartbeat.log').open('a') as output:output.write(str(time.time())+'\\n')
 time.sleep(1)
"""
    service = '[Unit]\nDescription=Disposable independent workload\nAfter=network.target\n[Service]\nUser=dev\nExecStart=/usr/bin/python3 /home/dev/spike-heartbeat.py\nRestart=always\n[Install]\nWantedBy=multi-user.target\n'
    setup = 'from pathlib import Path;import subprocess;Path("/home/dev/spike-heartbeat.py").write_text(' + repr(heartbeat) + ');Path("/etc/systemd/system/homecloud-spike-heartbeat.service").write_text(' + repr(service) + ');subprocess.run(["systemctl","daemon-reload"],check=True);subprocess.run(["systemctl","enable","--now","homecloud-spike-heartbeat.service"],check=True)'
    guest_command(vmid, 'python3 -c ' + shlex.quote(setup))
    before = t3_status(vmid)
    time.sleep(8)
    disconnected = t3_status(vmid)
    if before[0] != 'active' or disconnected[0] != 'active' or int(disconnected[-1]) <= int(before[-1]):
        raise RuntimeError('Service or workload did not continue across client disconnection')
    task(api('POST', vm_path(vmid) + '/status/reboot', {'timeout': 60}), timeout=180)
    wait_guest(vmid)
    rebooted = t3_status(vmid)
    time.sleep(3)
    reboot_progress = t3_status(vmid)
    if rebooted[-2] == before[-2] or int(reboot_progress[-1]) <= int(rebooted[-1]):
        raise RuntimeError('T3 boot service did not recover on a new guest boot')
    task(api('POST', '/nodes/' + node() + '/vzdump', {
        'vmid': str(vmid), 'storage': 'local', 'mode': 'snapshot', 'compress': 'zstd',
    }), timeout=300)
    backups = api('GET', '/nodes/' + node() + '/storage/local/content', {'content': 'backup', 'vmid': vmid})
    archives = sorted(entry['volid'] for entry in backups if entry.get('vmid') == vmid)
    if not archives:
        raise RuntimeError('No backup archive found')
    configuration = api('GET', vm_path(vmid) + '/config')
    if configuration.get('description') != TOKEN:
        raise RuntimeError('Refusing to replace foreign VM')
    task(api('POST', vm_path(vmid) + '/status/shutdown', {'timeout': 60}))
    task(api('DELETE', vm_path(vmid), {'purge': 1}))
    task(api('POST', '/nodes/' + node() + '/qemu', {
        'vmid': vmid, 'archive': archives[-1], 'storage': 'hc-alice', 'pool': 'hc-team', 'unique': 1,
    }), timeout=300)
    restored_configuration = api('GET', vm_path(vmid) + '/config')
    if not restored_configuration['scsi0'].startswith('hc-alice:'):
        raise RuntimeError('Restore escaped the tenant storage')
    task(api('POST', vm_path(vmid) + '/status/start'))
    wait_guest(vmid)
    restored = t3_status(vmid)
    time.sleep(3)
    restore_progress = t3_status(vmid)
    proof = guest_command(vmid, 'cat /home/dev/spike-proof')
    if proof != TOKEN or int(restore_progress[-1]) <= int(restored[-1]):
        raise RuntimeError('Restored environment did not recover its service and data')
    providers = guest_command(vmid, 'set -e; runuser -u dev -- /home/dev/.local/bin/codex --version; runuser -u dev -- /home/dev/.local/bin/claude --version').splitlines()
    credential = json.loads((DIRECTORY / 'alice-token.json').read_text())
    try:
        before_rebind = {'http_status': 200, 'status': api('GET', vm_path(vmid) + '/status/current', credential=credential)['status']}
    except urllib.error.HTTPError as error:
        if error.code != 403:
            raise
        before_rebind = {'http_status': 403}
    api('PUT', '/access/acl', {'path': '/vms/9001', 'users': 'hc-alice@pve', 'roles': 'PVEVMUser'})
    after_rebind = api('GET', vm_path(vmid) + '/status/current', credential=credential)['status']
    other_vm = expect_denied('GET', vm_path(9002) + '/config', credential=credential)
    if after_rebind != 'running' or other_vm.get('http_status') != 403:
        raise RuntimeError('Restored environment authorization did not reconcile')
    return {'before': before, 'after_client_disconnect': disconnected, 'after_guest_reboot': rebooted,
            'after_backup_restore': restored, 'restored_disk': restored_configuration['scsi0'],
            'reboot_workload_progress': reboot_progress, 'restore_workload_progress': restore_progress,
            'provider_versions': providers, 'restored_cpu_model': restored_configuration.get('cpu'),
            'restore_authorization': {'before_rebind': before_rebind, 'after_rebind': after_rebind, 'other_vm_http_status': other_vm['http_status']},
            'data_preserved': True, 't3_connect_authorized': False, 'provider_authorized': False,
            'cpu_transition': json.loads(transition_path.read_text()) if transition_path.exists() else None,
            'limitation': 'Real T3 service and independent workload tested across closed management calls; no authenticated T3 client session, user login, or actual AI agent task was performed.'}


def main():
    if ACTION in ('storage', 'templates', 't3'):
        wait_services()
    if ACTION == 'install':
        result = install()
    elif ACTION == 'probe':
        result = probe()
    elif ACTION == 'storage':
        result = storage_test()
    elif ACTION == 'templates':
        result = templates_test()
    elif ACTION == 't3':
        result = t3_test()
    else:
        raise RuntimeError('Guest action not implemented: ' + ACTION)
    print(json.dumps(result))


main()
