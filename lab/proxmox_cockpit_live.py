"""Disposable real-PVE validation; never install Proxmox on the physical host."""

import argparse
import base64
import concurrent.futures
import fcntl
import hashlib
import io
import json
from pathlib import Path
import shlex
import subprocess
import tarfile
import threading
import time
import uuid

from control_runner import atomic_json
import proxmox_spike

ROOT = Path(__file__).resolve().parents[1]
LAB = Path(__file__).resolve().parent
PIN = '89c1d597a9d15743049eadcb1add380573265904'
STATE_LOCK = threading.Lock()


def source_archive():
    files = [ROOT / 'Cargo.toml', ROOT / 'Cargo.lock']
    files += sorted((ROOT / 'crates').rglob('Cargo.toml'))
    files += sorted((ROOT / 'crates').rglob('build.rs'))
    for directory in sorted((ROOT / 'crates').glob('*/src')):
        files += sorted(candidate for candidate in directory.rglob('*') if candidate.is_file())
    hashes = {}
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w:gz') as archive:
        for filename in dict.fromkeys(files):
            if filename.is_symlink():
                raise RuntimeError('Refusing symlink in source snapshot')
            relative = str(filename.relative_to(ROOT))
            content = filename.read_bytes()
            hashes[relative] = hashlib.sha256(content).hexdigest()
            entry = tarfile.TarInfo(relative)
            entry.size = len(content)
            entry.mode = 0o600
            archive.addfile(entry, io.BytesIO(content))
    if PIN not in (ROOT / 'crates/virtualization/Cargo.toml').read_text():
        raise RuntimeError('Official client pin changed; review before running')
    return base64.b64encode(buffer.getvalue()).decode(), hashes


def helper(args, state, action, timeout=180, **extra):
    program = (LAB / 'proxmox_cockpit_host.py').read_text()
    request = {'state': state, 'action': action, **extra}
    command = shlex.join(['sudo', '-n', 'python3', '-c', program])
    result = subprocess.run(
        ['ssh', '-F', args.ssh_config, '-o', 'BatchMode=yes', 'hclab-host', command],
        input=json.dumps(request), text=True, capture_output=True, timeout=timeout,
    )
    if result.returncode:
        raise RuntimeError(result.stderr[-3500:] or result.stdout[-3500:])
    return json.loads(result.stdout)


def phase(args, state, action):
    if action in ('prepare', 'reboot'):
        return proxmox_spike.remote(args, {'state': state, 'action': action}, timeout=950)
    if action in ('install', 'seed'):
        program = (LAB / ('proxmox_guest.py' if action == 'install' else 'proxmox_cockpit_guest.py')).read_text()
        return proxmox_spike.remote(args, {
            'state': state, 'action': 'guest', 'guest_action': action,
            'program': program, 'timeout': 2400,
        }, timeout=2440)
    if action == 'build':
        archive, hashes = source_archive()
        with STATE_LOCK:
            state['source_sha256'] = hashes
            atomic_json(Path(args.state), state)
        return helper(args, state, action, timeout=2400, archive=archive,
                      probe=(LAB / 'proxmox_cockpit_probe.rs').read_text())
    if action == 'validate':
        return helper(args, state, action, timeout=300)
    if action == 'cleanup':
        helper(args, state, 'stop-build', timeout=180)
        result = proxmox_spike.remote(args, {
            'state': state, 'action': 'cleanup', 'force_worker_stop': True,
        }, timeout=400)
        result.update(helper(args, state, 'verify-cleanup', timeout=180))
        return result
    raise RuntimeError('Unknown phase')


def record(args, state, action):
    print(action + ': starting', flush=True)
    result = phase(args, state, action)
    with STATE_LOCK:
        state['observations'][action] = result
        atomic_json(Path(args.state), state)
    print(action + ': ' + json.dumps({key: value for key, value in result.items() if key != 'baseline_after'}), flush=True)
    return result


def assess(result):
    failures = []
    cases = result['cases']
    if not isinstance(result.get('probe_uid'), int) or result['probe_uid'] <= 0:
        failures.append('Production probe did not run as an unprivileged user')
    for name in ('reader', 'trusted-ca', 'trusted-ca-bad-fingerprint', 'iroh', 'iroh-trusted-ca', 'noaccess'):
        if not cases[name]['ok']:
            failures.append(name + ': ' + cases[name].get('error', 'failed'))
    for name in ('invalid', 'bad-fingerprint', 'untrusted-ca', 'public-secret'):
        if cases[name]['ok']:
            failures.append(name + ': unexpectedly accepted')
    expected_errors = {
        'invalid': ('authentication', 'HTTP 401'),
        'bad-fingerprint': ('connection failed',),
        'untrusted-ca': ('connection failed',),
        'public-secret': ('private regular file',),
    }
    for name, fragments in expected_errors.items():
        if not cases[name]['ok'] and not any(fragment in cases[name].get('error', '') for fragment in fragments):
            failures.append(name + ': failed for an unexpected reason')
    if cases['reader']['ok']:
        inventory = cases['reader']['result']['inventory']
        if inventory['source'] != 'proxmox':
            failures.append('Real adapter mislabeled its source')
        expected = {
            'nodes': sorted(entry['node'] for entry in result['reference_resources'] if entry['type'] == 'node'),
            'guests': sorted(entry['vmid'] for entry in result['reference_resources'] if entry['type'] in ('qemu', 'lxc')),
            'storage': sorted((entry['node'], entry['storage']) for entry in result['reference_resources'] if entry['type'] == 'storage'),
        }
        actual = {
            'nodes': sorted(entry['name'] for entry in inventory['nodes']),
            'guests': sorted(entry['vmid'] for entry in inventory['guests']),
            'storage': sorted((entry['node'], entry['name']) for entry in inventory['storage']),
        }
        if actual != expected:
            failures.append('Reference and adapter resource identities differ')
        nodes = {entry['name']: entry for entry in inventory['nodes']}
        storage = {(entry['node'], entry['name']): entry for entry in inventory['storage']}
        for resource in result['reference_resources']:
            if resource['type'] == 'node':
                mapped = nodes.get(resource['node'], {})
                for native, field in (('maxcpu', 'cpu_threads'), ('maxmem', 'memory_capacity_bytes')):
                    if mapped.get(field) != resource.get(native):
                        failures.append('Incorrect node field: ' + field)
            if resource['type'] == 'storage':
                mapped = storage.get((resource['node'], resource['storage']), {})
                if mapped.get('capacity_bytes') != resource.get('maxdisk'):
                    failures.append('Incorrect storage capacity: ' + resource['storage'])
                if mapped.get('status') != resource.get('status', 'unknown'):
                    failures.append('Incorrect storage status: ' + resource['storage'])
        guests = {entry['vmid']: entry for entry in inventory['guests']}
        native_guests = {entry['vmid']: entry for entry in result['reference_resources'] if entry['type'] in ('qemu', 'lxc')}
        for vmid, template in ((7101, False), (7199, True)):
            guest = guests.get(vmid, {})
            native_template = bool(native_guests.get(vmid, {}).get('template', False))
            if guest.get('template') != native_template:
                failures.append('Template mapping differs from native PVE response for ' + str(vmid))
            if native_template != template:
                failures.append('Native PVE template cache not settled for seeded VM ' + str(vmid))
            if (guest.get('status'), guest.get('kind'), guest.get('cpu_limit'),
                    guest.get('memory_limit_bytes'), guest.get('reported_disk_capacity_bytes')) != (
                    'stopped', 'virtual_machine', 2, 512 * 1024**2, 1024**3):
                failures.append('Incorrect mapping for seeded VM ' + str(vmid))
    if cases['noaccess']['ok']:
        inventory = cases['noaccess']['result']['inventory']
        if inventory['guests'] or inventory['storage']:
            failures.append('No-ACL privilege-separated token exposed guests or storage')
    for name in ('iroh', 'iroh-trusted-ca'):
        if not cases[name]['ok']:
            continue
        value = cases[name]['result']
        if not all(value.get(key) for key in ('authenticated_iroh', 'member_denied', 'unknown_denied', 'revoked_denied', 'legacy_denied')):
            failures.append('Authenticated Iroh authorization regression')
        if value.get('legacy_vms_created') != 0:
            failures.append('Inventory created legacy workload records')
        if cases['reader']['ok']:
            for kind, key in (('nodes', 'name'), ('guests', 'vmid'), ('storage', 'name')):
                direct = sorted(entry[key] for entry in cases['reader']['result']['inventory'][kind])
                routed = sorted(entry[key] for entry in value['inventory'][kind])
                if direct != routed:
                    failures.append('Iroh inventory identities differ: ' + kind)
    return {'passed': not failures, 'failures': failures}


def evidence(state):
    cleanup = state['observations'].get('cleanup', {})
    if not state.get('cleaned') or not cleanup.get('baseline_configuration_restored') or not cleanup.get('lab_resources_absent'):
        raise RuntimeError('Cannot publish evidence before exact cleanup verification of owned resources and host configuration')
    relevant = ('Cargo.toml', 'Cargo.lock', 'crates/virtualization/Cargo.toml',
                'crates/virtualization/src/proxmox.rs', 'crates/virtualization/src/resources.rs',
                'crates/daemon/src/lib.rs', 'crates/daemon/src/main.rs', 'crates/client/src/lib.rs')
    hashes = {name: state.get('source_sha256', {}).get(name) for name in relevant}
    observations = state['observations']
    validation = observations.get('validate')
    failure_lines = state.get('failure', 'Validation did not execute').strip().splitlines()
    baseline_checks = {key: value == cleanup.get('baseline_after', {}).get(key)
                       for key, value in state['baseline'].items()}
    return {
        'date': '2026-11-02', 'run_id': state['run_id'], 'physical_host': 'Ubuntu/Incus resolvr-box, not Proxmox',
        'started_at_unix': state.get('started_at_unix'), 'finished_at_unix': state.get('finished_at_unix'),
        'official_client_revision': state['official_client_revision'], 'source_sha256': hashes,
        'assessment': assess(validation) if validation else {'passed': False, 'failures': [failure_lines[-1] if failure_lines else 'Validation did not execute']},
        'seed': observations.get('seed'), 'build': observations.get('build'), 'validation': validation,
        'cleanup': {key: value for key, value in cleanup.items() if key != 'baseline_after'},
        'baseline_field_equality': baseline_checks,
        'original_vm_power_states': {name: entry['status'] for name, entry in state['baseline']['instances'].items()},
        'cpu_policy_sha256_before_and_after': state['baseline']['cpu_policy_sha256'],
        'sensitive_baseline_omitted': True,
    }


def execute(args):
    filename = Path(args.state)
    if args.action == 'report':
        if not args.output:
            raise RuntimeError('Report requires --output')
        summary = evidence(json.loads(filename.read_text()))
        summary['previous_runs'] = []
        for previous_filename in args.previous_state:
            previous = evidence(json.loads(Path(previous_filename).read_text()))
            summary['previous_runs'].append({
                'run_id': previous['run_id'], 'assessment': previous['assessment'], 'cleanup': previous['cleanup'],
                'baseline_field_equality': previous['baseline_field_equality'],
                'write_denied_http': (previous.get('seed') or {}).get('write_denied_http'),
                'api_case_outcomes': {name: {'ok': value['ok'], 'error': value.get('error')}
                                      for name, value in (previous.get('validation') or {}).get('cases', {}).items()},
            })
        atomic_json(Path(args.output), summary)
        print('Sanitized evidence: ' + args.output)
        return
    if filename.exists():
        state = json.loads(filename.read_text())
        if args.action != 'cleanup':
            raise RuntimeError('Existing manifest is cleanup-only; use a new manifest for a new run')
    else:
        if args.action != 'run':
            raise RuntimeError('No manifest to clean')
        token = uuid.uuid4().hex
        state = {
            'version': 1, 'run_id': token, 'instance': 'hc-pve-' + token[:10],
            'storage': 'hcpve-' + token[:10], 'network': 'hcvmsp' + token[:8],
            'dataset': 'hclab/proxmox-spike-' + token, 'observations': {},
            'started_at_unix': time.time(), 'official_client_revision': PIN,
        }
        state['baseline'] = helper(args, state, 'baseline')
        state['controller_was_active'] = state['baseline']['controller'] == 'active'
        state['original_vms'] = []
        atomic_json(filename, state)
    if args.action == 'cleanup':
        record(args, state, 'cleanup')
        state['cleaned'] = True
        atomic_json(filename, state)
        return
    failure = None
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as workers:
            build = workers.submit(record, args, state, 'build')
            try:
                for action in ('prepare', 'install', 'reboot', 'seed'):
                    record(args, state, action)
            finally:
                build.result()
        result = record(args, state, 'validate')
        state['assessment'] = assess(result)
        atomic_json(filename, state)
        if not state['assessment']['passed']:
            raise RuntimeError('; '.join(state['assessment']['failures']))
    except BaseException as error:
        failure = error
        state['failure'] = str(error)
        atomic_json(filename, state)
    finally:
        record(args, state, 'cleanup')
        state['cleaned'] = True
        state['finished_at_unix'] = time.time()
        atomic_json(filename, state)
    if failure:
        raise failure


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['run', 'cleanup', 'report'])
    parser.add_argument('--state', required=True)
    parser.add_argument('--output')
    parser.add_argument('--previous-state', action='append', default=[])
    parser.add_argument('--ssh-config', default=str(Path.home() / '.config/homecloud/host-access/resolvr-box/ssh_config'))
    args = parser.parse_args()
    filename = Path(args.state)
    filename.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (filename.parent / '.proxmox-cockpit-live.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        execute(args)


if __name__ == '__main__':
    main()
