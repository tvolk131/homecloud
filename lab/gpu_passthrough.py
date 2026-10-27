#!/usr/bin/env python3
"""Explicitly authorized, disposable lab GPU handoff; never a tenant policy."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

VM = 'hclab-vm02'
GPU = '0000:01:00.0'
AUDIO = '0000:01:00.1'
ROOT = Path('/var/lib/home-cloud-lab')
STATE = ROOT / 'gpu-handoff-state.json'
SELF = '/usr/local/libexec/home-cloud-lab/gpu_passthrough.py'


def run(*args, check=True, timeout=60):
    p = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if check and p.returncode:
        raise RuntimeError(f'{args!r}: {p.stdout}\n{p.stderr}')
    return p


def query(path):
    return json.loads(run('incus', 'query', path).stdout)


def pci(address):
    return Path('/sys/bus/pci/devices') / address


def driver(address):
    link = pci(address) / 'driver'
    return link.resolve().name if link.exists() else None


def save(data):
    STATE.write_text(json.dumps(data, indent=2) + '\n')


def guard():
    assert os.geteuid() == 0
    assert (pci(GPU) / 'vendor').read_text().strip() == '0x10de'
    assert (pci(GPU) / 'device').read_text().strip() == '0x1b82'
    assert (pci(AUDIO) / 'device').read_text().strip() == '0x10f0'
    group = (pci(GPU) / 'iommu_group').resolve(strict=True)
    assert {p.name for p in (group / 'devices').iterdir()} == {
        '0000:00:01.0', GPU, AUDIO}, 'Unexpected IOMMU group membership'
    details = query(f'/1.0/instances/{VM}')
    assert details['config'].get('user.home-cloud-lab') == 'true'
    assert details['config'].get('boot.autostart') == 'false'
    return details


def release():
    details = guard()
    state = json.loads(STATE.read_text())
    if query(f'/1.0/instances/{VM}/state')['status'] == 'Running':
        p = run('incus', 'stop', VM, '--timeout', '30', check=False, timeout=60)
        if p.returncode:
            # This is only our disposable test VM, with no coworker workloads.
            run('incus', 'stop', VM, '--force', timeout=30)
    if 'gpu0' in details['devices']:
        assert details['devices']['gpu0'].get('pci') == GPU
        run('incus', 'config', 'device', 'remove', VM, 'gpu0')
    for address, original in state['original_drivers'].items():
        if driver(address) != original:
            if driver(address):
                (pci(address) / 'driver/unbind').write_text(address)
            (pci(address) / 'driver_override').write_text(original or '\n')
            if original:
                run('modprobe', original)
            Path('/sys/bus/pci/drivers_probe').write_text(address)
        (pci(address) / 'driver_override').write_text('\n')
    for name in state['bound_vtconsoles']:
        bind = Path('/sys/class/vtconsole') / name / 'bind'
        if bind.exists():
            try:
                bind.write_text('1')
            except OSError as error:
                state.setdefault('restore_warnings', []).append(str(error))
    for unit in state['active_gettys']:
        run('systemctl', 'start', unit, check=False)
    if state.get('recovery_timer'):
        run('systemctl', 'stop', state['recovery_timer'], check=False)
    state['released'] = True
    state['restored_drivers'] = {address: driver(address) for address in (GPU, AUDIO)}
    save(state)
    print(json.dumps(state, indent=2), flush=True)


def attach():
    details = guard()
    assert query(f'/1.0/instances/{VM}/state')['status'] == 'Stopped'
    assert driver(GPU) == 'nouveau' and driver(AUDIO) == 'snd_hda_intel'
    assert 'gpu0' not in details['devices']
    assert not run('fuser', '/dev/dri/card0', '/dev/dri/renderD128', check=False).stdout.strip(), 'GPU has userspace clients'
    state = {
        'vm': VM, 'gpu': GPU,
        'original_drivers': {address: driver(address) for address in (GPU, AUDIO)},
        'bound_vtconsoles': [p.name for p in Path('/sys/class/vtconsole').glob('vtcon*')
                             if (p / 'bind').read_text().strip() == '1'],
        'active_gettys': [unit for unit in ['getty@tty1.service']
                          if run('systemctl', 'is-active', unit, check=False).returncode == 0],
        'released': False,
    }
    ROOT.mkdir(exist_ok=True)
    save(state)
    timer = 'home-cloud-gpu-recovery-' + str(int(time.time()))
    run('systemd-run', '--collect', '--unit=' + timer, '--on-active=15min',
        '/usr/bin/python3', SELF, 'release')
    state['recovery_timer'] = timer + '.timer'
    save(state)
    try:
        run('incus', 'config', 'device', 'add', VM, 'gpu0', 'gpu',
            'gputype=physical', 'pci=' + GPU)
        for unit in state['active_gettys']:
            run('systemctl', 'stop', unit)
        for name in state['bound_vtconsoles']:
            (Path('/sys/class/vtconsole') / name / 'bind').write_text('0')
        print('Host console detached; asking Incus to hand over the GPU', flush=True)
        run('incus', 'start', VM, timeout=120)
        state['assigned_drivers'] = {address: driver(address) for address in (GPU, AUDIO)}
        assert all(value == 'vfio-pci' for value in state['assigned_drivers'].values()), state
        save(state)
        print(json.dumps(state, indent=2), flush=True)
    except Exception:
        try:
            release()
        finally:
            raise


if __name__ == '__main__':
    {'attach': attach, 'release': release}[sys.argv[1]]()
