#!/usr/bin/env python3
"""Provision only the explicitly authorized WD test disk; run as root."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

DISK = Path('/dev/disk/by-id/ata-WDC_WD10EZEX-08WN4A0_WD-WCC6Y1CL8EKY')
SERIAL = 'WD-WCC6Y1CL8EKY'
POOL = 'hclab'
EVIDENCE = Path('/var/lib/home-cloud-lab')


def run(*args, check=True):
    result = subprocess.run(args, capture_output=True, text=True, timeout=120)
    if check and result.returncode:
        raise RuntimeError(f'{args!r}: {result.stdout}\n{result.stderr}')
    return result


def descendants(node):
    yield node
    for child in node.get('children', []):
        yield from descendants(child)


if os.geteuid() != 0:
    sys.exit('Run as root')
lock = open('/run/lock/home-cloud-lab-storage.lock', 'w')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
disk = DISK.resolve(strict=True)
inventory = json.loads(run('lsblk', '-J', '-b', '-o',
    'NAME,PATH,SIZE,TYPE,FSTYPE,MOUNTPOINTS,MODEL,SERIAL', str(disk)).stdout)
node = inventory['blockdevices'][0]
assert node['type'] == 'disk' and node['serial'] == SERIAL, inventory
assert node['model'].startswith('WDC WD10EZEX'), inventory
assert 999_000_000_000 < node['size'] < 1_001_000_000_000, inventory
nodes = list(descendants(node))
assert all(not any(n.get('mountpoints', [])) for n in nodes), inventory
for n in nodes:
    assert not list((Path('/sys/class/block') / n['name'] / 'holders').iterdir()), n

health = run('smartctl', '-x', '-j', str(DISK), check=False)
smart = json.loads(health.stdout)
assert health.returncode == 0, smart
assert smart['serial_number'] == SERIAL and smart['smart_status']['passed'], smart
for attribute in smart.get('ata_smart_attributes', {}).get('table', []):
    if attribute['id'] in (5, 197, 198):
        assert attribute['raw']['value'] == 0, attribute

existing = run('zpool', 'list', '-H', '-o', 'name', check=False).stdout.split()
if POOL in existing:
    status = run('zpool', 'status', '-P', POOL).stdout
    assert DISK.name in status, status
    print('Existing authorized hclab pool retained', flush=True)
else:
    imported = run('zpool', 'import', '-d', '/dev/disk/by-id').stdout
    assert SERIAL not in imported, imported
    print(f'Erasing authorized disk {DISK} ({SERIAL})', flush=True)
    for n in reversed(nodes):
        run('wipefs', '--all', n['path'])
    run('udevadm', 'settle')
    run('zpool', 'create', '-o', 'ashift=12', '-o',
        'cachefile=/etc/zfs/zpool.cache', '-O', 'compression=lz4',
        '-O', 'atime=off', '-O', 'canmount=off', '-m', 'none', POOL, str(DISK))

def dataset(name, *properties):
    if run('zfs', 'list', '-H', name, check=False).returncode:
        args = ['zfs', 'create']
        for prop in properties:
            args.extend(['-o', prop])
        run(*args, name)

dataset('hclab/emergency-reserve', 'mountpoint=none', 'canmount=off', 'reservation=50G')
dataset('hclab/tenants', 'mountpoint=none', 'canmount=off')
dataset('hclab/tenants/lab', 'mountpoint=none', 'canmount=off', 'quota=64G', 'reservation=64G')
dataset('hclab/tenants/lab/incus', 'mountpoint=none', 'canmount=off')

EVIDENCE.mkdir(mode=0o755, parents=True, exist_ok=True)
test_mount = EVIDENCE / 'storage-smoke'
dataset('hclab/storage-smoke', f'mountpoint={test_mount}', 'canmount=on', 'quota=1G')
payload = test_mount / 'integrity.bin'
expected = hashlib.sha256()
with payload.open('wb') as out:
    for _ in range(128):
        block = os.urandom(1024 * 1024)
        out.write(block)
        expected.update(block)
    out.flush()
    os.fsync(out.fileno())
run('zpool', 'sync', POOL)
actual = hashlib.sha256()
with payload.open('rb') as inp:
    for block in iter(lambda: inp.read(1024 * 1024), b''):
        actual.update(block)
assert actual.digest() == expected.digest(), 'Storage integrity check failed'
payload.unlink()
run('zfs', 'destroy', 'hclab/storage-smoke')
print('128 MiB write/read SHA-256 check passed', flush=True)

# No instances exist yet, so this also verifies that the pool can be reimported.
run('zpool', 'export', POOL)
run('zpool', 'import', '-d', '/dev/disk/by-id', '-o',
    'cachefile=/etc/zfs/zpool.cache', POOL)
print('Pool export/import passed', flush=True)

arc_config = Path('/etc/modprobe.d/home-cloud-lab-zfs.conf')
arc_config.write_text('# Home Cloud lab: retain RAM for guests and QEMU.\noptions zfs zfs_arc_max=2147483648\n')
Path('/sys/module/zfs/parameters/zfs_arc_max').write_text('2147483648\n')

evidence = {
    'disk_before': inventory,
    'smart_before': smart,
    'smoke_bytes': 128 * 1024 * 1024,
    'smoke_sha256': actual.hexdigest(),
    'export_import_passed': True,
    'zpool_status': run('zpool', 'status', '-P', POOL).stdout,
    'datasets': run('zfs', 'list', '-p', '-o', 'name,used,available,quota,reservation,mountpoint', '-r', POOL).stdout,
    'zfs_arc_max': Path('/sys/module/zfs/parameters/zfs_arc_max').read_text().strip(),
    'zfs_version': run('zfs', 'version').stdout,
}
(EVIDENCE / 'storage.json').write_text(json.dumps(evidence, indent=2) + '\n')
print(json.dumps({k: v for k, v in evidence.items() if k not in ('disk_before', 'smart_before')}, indent=2))
