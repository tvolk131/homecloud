"""Bounded task-tracked power experiment, using the existing disposable harness."""

import argparse
import concurrent.futures
import fcntl
import hashlib
import json
from pathlib import Path
import shlex
import subprocess
import time
import uuid

from control_runner import atomic_json
import proxmox_cockpit_live as cockpit
import proxmox_spike

LAB = Path(__file__).resolve().parent


def wrapped(module, extension):
    return ('import sys, types\nbase = types.ModuleType(' + repr(module) + ')\n'
            'sys.modules[' + repr(module) + '] = base\nexec(' + repr((LAB / (module + '.py')).read_text())
            + ', base.__dict__)\n' + (LAB / extension).read_text())


def helper(args, state, action, timeout=180, **extra):
    program = wrapped('proxmox_cockpit_host', 'proxmox_power_host.py')
    command = shlex.join(['sudo', '-n', 'python3', '-c', program])
    result = subprocess.run(['ssh', '-F', args.ssh_config, '-o', 'BatchMode=yes', 'hclab-host', command],
                            input=json.dumps({'state': state, 'action': action, **extra}),
                            text=True, capture_output=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError((result.stderr or result.stdout)[-4000:])
    return json.loads(result.stdout)


def assess(value):
    failures = []
    required = ('start', 'shutdown', 'resume', 'force-stop', 'reader', 'invalid', 'template',
                'seed-hold', 'hold', 'authorization')
    for name in required:
        if not value.get('cases', {}).get(name, {}).get('ok'):
            failures.append(name + ': missing or failed')
    if value.get('failure'):
        failures.append(value['failure'])
    if value.get('probe_uid', 0) <= 0 or not value.get('token_values_redacted'):
        failures.append('Probe privilege or secret handling failed')
    native = value.get('native', {})
    if not failures:
        initial = native['before']['power_tasks']
        final = native['authorization']['power_tasks']
        new = {entry['upid']: entry for entry in final if entry['upid'] not in {entry['upid'] for entry in initial}}
        expected = {value['cases'][name]['result']['observations']['running']['task_id'] for name in ('start', 'shutdown', 'force-stop')}
        if set(new) != expected or sorted(entry['type'] for entry in new.values()) != ['qmshutdown', 'qmstart', 'qmstop']:
            failures.append('Native power task identities/count differ; possible duplicate dispatch')
        if native['resume']['qm_status']['7101'] != 'running':
            failures.append('Graceful timeout unexpectedly stopped the no-OS VM')
        if native['start']['qm_status'] != {'7101': 'running', '7199': 'stopped'}:
            failures.append('Start did not produce native running state')
        if native['force-stop']['qm_status'] != {'7101': 'stopped', '7199': 'stopped'}:
            failures.append('Explicit force-stop/native template state mismatch')
        for name in ('reader', 'invalid', 'template', 'seed-hold', 'hold', 'authorization'):
            if {entry['upid'] for entry in native[name]['power_tasks']} != set(new) | {entry['upid'] for entry in initial}:
                failures.append(name + ': negative test dispatched a native task')
            if native[name]['qm_status'] != {'7101': 'stopped', '7199': 'stopped'}:
                failures.append(name + ': negative test mutated power state')
        if value['cases']['resume']['result']['observations']['terminal']['phase'] != 'failed':
            failures.append('No-OS graceful shutdown did not record task failure')
        for name, checkpoint in (('start', 'start'), ('shutdown', 'resume'), ('force-stop', 'force-stop')):
            upid = value['cases'][name]['result']['observations']['running']['task_id']
            task = native[checkpoint]['task_status'].get(upid, {})
            expected_success = name != 'shutdown'
            if task.get('status') != 'stopped' or not isinstance(task.get('exitstatus'), str) or (task.get('exitstatus') == 'OK') != expected_success:
                failures.append(name + ': native task completion contradicts operation result')
        shutdown_upid = value['cases']['shutdown']['result']['observations']['running']['task_id']
        if native['shutdown']['task_status'].get(shutdown_upid, {}).get('status') != 'running':
            failures.append('Shutdown was not running at process restart boundary')
        for name, result in value['cases'].items():
            if name != 'seed-hold' and result['result'].get('legacy_vms_created') != 0:
                failures.append(name + ': created legacy VM records')
    return {'passed': not failures, 'failures': failures}


def evidence(state):
    cleanup = state['observations'].get('cleanup', {})
    if not state.get('cleaned') or not all(cleanup.get(key) for key in (
            'baseline_configuration_restored', 'lab_resources_absent', 'transient_units_absent', 'power_probe_unit_absent')):
        raise RuntimeError('Refusing publication without independent cleanup verification')
    validation = state['observations'].get('validate')
    return {'date': '2026-11-02', 'run_id': state['run_id'], 'official_client_revision': cockpit.PIN,
            'physical_host': 'Ubuntu/Incus resolvr-box, not Proxmox',
            'started_at_unix': state['started_at_unix'], 'finished_at_unix': state.get('finished_at_unix'),
            'source_sha256': state.get('source_sha256', {}), 'harness_sha256': state.get('harness_sha256', {}),
            'assessment': assess(validation) if validation else {'passed': False, 'failures': [state.get('failure', 'No live validation')]},
            'seed': state['observations'].get('power-seed'), 'build': state['observations'].get('build'),
            'validation': validation, 'failure': state.get('failure'),
            'cleanup': {key: value for key, value in cleanup.items() if key != 'baseline_after'},
            'baseline_field_equality': {key: value == cleanup.get('baseline_after', {}).get(key) for key, value in state['baseline'].items()},
            'original_vm_power_states': {name: entry['status'] for name, entry in state['baseline']['instances'].items()},
            'sensitive_baseline_omitted': True}


def phase(args, state, action):
    if action == 'power-seed':
        return proxmox_spike.remote(args, {'state': state, 'action': 'guest', 'guest_action': action,
            'program': wrapped('proxmox_cockpit_guest', 'proxmox_power_guest.py'), 'timeout': 180}, timeout=220)
    if action == 'build':
        archive, hashes = cockpit.source_archive()
        with cockpit.STATE_LOCK:
            state['source_sha256'] = hashes
            state['harness_sha256'] = {filename.name: hashlib.sha256(filename.read_bytes()).hexdigest()
                                       for filename in sorted(LAB.glob('proxmox_power*')) if filename.is_file()}
            atomic_json(Path(args.state), state)
        return helper(args, state, 'build', timeout=2400, archive=archive,
                      probe=(LAB / 'proxmox_power_probe.rs').read_text())
    if action == 'validate':
        return helper(args, state, action, timeout=600)
    if action == 'cleanup':
        try:
            probe = helper(args, state, 'stop-probe')
        finally:
            cleanup = cockpit.phase(args, state, action)
        return {**cleanup, **probe}
    return cockpit.phase(args, state, action)


def execute(args):
    filename = Path(args.state)
    if args.action == 'report':
        if not args.output:
            raise RuntimeError('Report needs --output')
        summary = evidence(json.loads(filename.read_text()))
        summary['previous_runs'] = []
        for previous_filename in args.previous_state:
            previous = evidence(json.loads(Path(previous_filename).read_text()))
            summary['previous_runs'].append({key: previous[key] for key in (
                'run_id', 'assessment', 'failure', 'cleanup', 'baseline_field_equality',
                'source_sha256', 'harness_sha256')})
        atomic_json(Path(args.output), summary)
        return
    if filename.exists():
        if args.action != 'cleanup':
            raise RuntimeError('Existing manifest is cleanup-only; use a fresh manifest')
        state = json.loads(filename.read_text())
    else:
        if args.action != 'run':
            raise RuntimeError('No existing manifest to clean')
        token = uuid.uuid4().hex
        state = {'version': 1, 'run_id': token, 'instance': 'hc-pve-' + token[:10],
                 'storage': 'hcpve-' + token[:10], 'network': 'hcvmsp' + token[:8],
                 'dataset': 'hclab/proxmox-spike-' + token, 'observations': {},
                 'started_at_unix': time.time(), 'official_client_revision': cockpit.PIN, 'original_vms': []}
        state['baseline'] = cockpit.helper(args, state, 'baseline')
        state['controller_was_active'] = state['baseline']['controller'] == 'active'
        atomic_json(filename, state)

    def record(action):
        print(action + ': starting', flush=True)
        value = phase(args, state, action)
        with cockpit.STATE_LOCK:
            state['observations'][action] = value
            atomic_json(filename, state)
        print(action + ': complete', flush=True)
        return value

    try:
        if args.action == 'run':
            record('prepare')
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as workers:
                building = workers.submit(record, 'build')
                try:
                    for action in ('install', 'reboot', 'seed', 'power-seed'):
                        record(action)
                finally:
                    building.result()
            assessment = assess(record('validate'))
            state['assessment'] = assessment
            if not assessment['passed']:
                raise RuntimeError('; '.join(assessment['failures']))
    except BaseException as error:
        state['failure'] = str(error)
        atomic_json(filename, state)
        raise
    finally:
        record('cleanup')
        state['cleaned'] = True
        state['finished_at_unix'] = time.time()
        atomic_json(filename, state)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('run', 'cleanup', 'report'))
    parser.add_argument('--state', required=True)
    parser.add_argument('--output')
    parser.add_argument('--previous-state', action='append', default=[])
    parser.add_argument('--ssh-config', default=str(Path.home() / '.config/homecloud/host-access/resolvr-box/ssh_config'))
    args = parser.parse_args()
    filename = Path(args.state)
    filename.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (filename.parent / '.proxmox-power-live.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        execute(args)


if __name__ == '__main__':
    main()
