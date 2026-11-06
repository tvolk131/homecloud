"""Wrap the original ownership-safe host harness; never install host packages."""

import hashlib
import json
import os
from pathlib import Path
import pwd
import sys
import time

import proxmox_cockpit_host as cockpit


def build(request, directory, token):
    result = cockpit.build(request, directory, token)
    binaries = {name: directory / 'target/debug' / name for name in ('homecloud-cockpit-live-probe', 'homecloudd')}
    if any(not binary.is_file() or not os.access(binary, os.X_OK) for binary in binaries.values()):
        raise RuntimeError('Isolated build stopped before producing both required executable binaries')
    if not any(line.startswith('test result: ok.') for line in result['test_output']) or 'proxmox-client' not in result['tests_and_build']:
        raise RuntimeError('Isolated build has no completed test/client dependency proof')
    result['binary_sha256'] = {name: hashlib.sha256(binary.read_bytes()).hexdigest() for name, binary in binaries.items()}
    result['required_binaries_verified'] = True
    return result


def native(state):
    cockpit.worker(state)
    token = state['run_id']
    node = state['instance']
    statuses = {}
    for vmid in (7101, 7199):
        config = cockpit.guest(state, 'qm', 'config', str(vmid))
        if 'description: Owned disposable run ' + token not in config:
            raise RuntimeError('Refusing nonowned nested VM')
        statuses[str(vmid)] = cockpit.guest(state, 'qm', 'status', str(vmid)).removeprefix('status: ')
    tasks = json.loads(cockpit.guest(state, 'pvesh', 'get', '/nodes/' + node + '/tasks',
                                    '--vmid', '7101', '--source', 'all', '--limit', '100', '--output-format', 'json'))
    tasks = [entry for entry in tasks if entry['type'] in ('qmstart', 'qmshutdown', 'qmstop')]
    details = {entry['upid']: json.loads(cockpit.guest(state, 'pvesh', 'get',
                '/nodes/' + node + '/tasks/' + entry['upid'] + '/status', '--output-format', 'json'))
               for entry in tasks}
    return {'qm_status': statuses, 'power_tasks': tasks, 'task_status': details}


def settled(state, expected):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        resources = json.loads(cockpit.guest(state, 'pvesh', 'get', '/cluster/resources',
                                            '--type', 'vm', '--output-format', 'json'))
        target = next((entry for entry in resources if entry.get('vmid') == 7101), {})
        if target.get('status') == expected:
            return
        time.sleep(1)
    raise RuntimeError('Native inventory status did not settle: ' + expected)


def validate(state, directory):
    cockpit.owned(directory, state['run_id'])
    private = directory / 'power-validation'
    private.mkdir(mode=0o700)
    account = pwd.getpwnam('tommy')
    credentials = {name: json.loads(cockpit.guest(state, 'cat', '/root/homecloud-proxmox-spike/' + name + '-token.json'))
                   for name in ('power', 'reader')}
    fingerprint = cockpit.guest(state, 'cat', '/root/homecloud-proxmox-spike/tls-fingerprint')
    for name in ('power', 'reader', 'invalid'):
        target = private / name
        target.mkdir(mode=0o700)
        credential = credentials['power' if name == 'power' else 'reader']
        secret = target / 'secret'
        secret.write_text('invalid-disposable-power-token' if name == 'invalid' else credential['value'])
        secret.chmod(0o600)
        config = {'endpoint': 'https://172.31.246.2:8006/', 'token_id': credential['full-tokenid'],
                  'token_secret_file': str(secret), 'tls_fingerprint': fingerprint}
        (target / 'config.json').write_text(json.dumps(config))
        (target / 'fixture.json').write_text(json.dumps({'node': state['instance'], 'run_id': state['run_id']}))
        for filename in target.iterdir():
            filename.chmod(0o600)
    cockpit.run('chown', '-hR', str(account.pw_uid) + ':' + str(account.pw_gid), str(private))
    housekeeping = json.loads(Path('/etc/homecloud/host.json').read_text())['housekeeping']
    binary = directory / 'target/debug/homecloud-cockpit-live-probe'
    cases = {}
    observations = {'before': native(state)}

    def probe(mode, credential='power'):
        output = cockpit.run(
            'systemd-run', '--quiet', '--wait', '--pipe', '--collect', '--uid=tommy',
            '--unit=homecloud-power-probe-' + state['run_id'],
            '--property=AllowedCPUs=' + ','.join(map(str, housekeeping)),
            '--property=MemoryMax=1G', '--property=TasksMax=128', '--property=RuntimeMaxSec=140',
            str(binary), mode, str(private / credential / 'config.json'), timeout=160,
        ).stdout
        result = json.loads(output)
        if any(credential['value'] in json.dumps(result) for credential in credentials.values()):
            raise RuntimeError('Refusing to export a token secret')
        cases[mode] = result
        observations[mode] = native(state)
        if not result.get('ok'):
            raise RuntimeError(mode + ': ' + result.get('error', 'Probe failed'))
        return result

    failure = None
    try:
        probe('start')
        settled(state, 'running')
        probe('shutdown')
        probe('resume')
        terminal = cases['resume']['result']['observations']['terminal']['phase']
        status = observations['resume']['qm_status']['7101']
        if (terminal, status) not in (('failed', 'running'), ('succeeded', 'stopped')):
            raise RuntimeError('Graceful shutdown result disagrees with native state')
        if status != 'running':
            raise RuntimeError('Bare no-OS fixture unexpectedly shut down; cannot demonstrate explicit ForceStop')
        settled(state, 'running')
        probe('force-stop')
        settled(state, 'stopped')
        probe('reader', 'reader')
        probe('invalid', 'invalid')
        probe('template')
        probe('seed-hold')
        probe('hold')
        probe('authorization')
    except BaseException as error:
        failure = str(error)
    finally:
        cockpit.run('systemctl', 'stop', 'homecloud-power-probe-' + state['run_id'] + '.service', check=False)
    value = {'cases': cases, 'native': observations, 'failure': failure,
             'probe_uid': account.pw_uid, 'token_values_redacted': True,
             'probe_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
             'pve_version': cockpit.guest(state, 'pveversion'),
             'kernel': cockpit.guest(state, 'uname', '-r'),
             'probe_unit_load_state': cockpit.run('systemctl', 'show', '--property=LoadState', '--value',
                'homecloud-power-probe-' + state['run_id'] + '.service', check=False).stdout.strip()}
    if any(credential['value'] in json.dumps(value) for credential in credentials.values()):
        raise RuntimeError('Secret present in validation observations')
    return value


def main():
    request = json.load(sys.stdin)
    state = request['state']
    token, directory = cockpit.identities(state)
    action = request['action']
    if action == 'build':
        result = build(request, directory, token)
    elif action == 'validate':
        result = validate(state, directory)
    elif action == 'stop-probe':
        cockpit.run('systemctl', 'stop', 'homecloud-power-probe-' + token + '.service', check=False)
        loaded = cockpit.run('systemctl', 'show', '--property=LoadState', '--value',
                            'homecloud-power-probe-' + token + '.service', check=False).stdout.strip()
        if loaded != 'not-found':
            raise RuntimeError('Power probe transient unit still loaded')
        result = {'power_probe_unit_absent': True, 'load_state': loaded}
    else:
        raise RuntimeError('Unknown power host action')
    print(json.dumps(result))


if __name__ == '__main__':
    main()
