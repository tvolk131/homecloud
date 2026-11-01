"""Fixed host-side managed-application hooks; guest jobs are supervised by systemd."""
import hashlib
import json
import subprocess
from pathlib import Path
import uuid
import managed_host

GUEST_DIR = '/usr/local/libexec/homecloud'
RUNNER = GUEST_DIR + '/homecloud-guest'
RUNNER_SOURCE = Path(__file__).with_name('homecloud-guest')
# Earlier releases ran Python guest helpers. Keep them while an application job
# may still need their paths, then retry cleanup on later inspections.
LEGACY = ('managed-app.py', 'setup-t3.sh', 'setup-providers.py', 'codex_auth.py', 'claude_auth.py',
          'github_auth.py', 'account_metadata.py')
SETUP_ACTIONS = ('connect_t3', 'sign_in_codex', 'sign_in_claude', 'sign_in_github', 'sign_out_github')


def install_runner(runtime, name):
    """Upgrade the fixed runner independently of the application installation.

    Inspecting an older VM must discover new account capabilities without
    reinstalling T3 or restarting a user's active sessions. The rename keeps a
    running job on the binary it started with.
    """
    digest = hashlib.sha256(RUNNER_SOURCE.read_bytes()).hexdigest()

    def root(*args, check=True):
        return runtime.run('incus', 'exec', name, '--', *args, timeout=10, check=check)

    existing = root('sha256sum', RUNNER, check=False).stdout.split()
    if not existing or existing[0] != digest:
        staged = RUNNER + '.new-' + uuid.uuid4().hex
        root('mkdir', '-p', '-m', '0755', GUEST_DIR)
        try:
            runtime.run('incus', 'file', 'push', '--uid', '0', '--gid', '0', '--mode', '0755',
                        str(RUNNER_SOURCE), name + staged, timeout=120)
            root('mv', '-f', '-T', staged, RUNNER)
        finally:
            try:root('rm', '-f', staged, check=False)
            except (OSError, RuntimeError, subprocess.SubprocessError):pass
    active = root('systemctl', 'list-units', '--type=service', '--state=activating,active,deactivating',
                  '--no-legend', '--plain', 'homecloud-app-*.service', check=False)
    if active.returncode != 0 or active.stdout.strip():return
    root('rm', '-f', *[GUEST_DIR + '/' + file for file in LEGACY])


def application(runtime, vm, app, action):
    if app['kind']=='homecloud_host':return managed_host.application(runtime,vm,app,action)
    assert app['vm_id'] == vm['id'] and app['kind'] == 't3'
    app_id = str(uuid.UUID(app['id']))
    assert app['desired_version'] == '0.1.2' and app['start_on_boot'] is True
    runtime.owned(vm)
    assert runtime.state(vm) == 'running', 'VM is not running'
    name = runtime.vm_name(vm)
    env = ['env', 'HOME=/home/dev', 'PATH=/home/dev/.local/bin:/usr/local/bin:/usr/bin:/bin',
           'XDG_RUNTIME_DIR=/run/user/1000', 'DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus']

    def guest(*args, timeout=65, check=True, input=None):
        return runtime.run('incus', 'exec', name, '--', 'runuser', '-u', 'dev', '--', *env, *args,
                           timeout=timeout, check=check, input=input)

    install_runner(runtime, name)
    report = json.loads(guest(RUNNER, 'inspect', app_id).stdout)
    operation = action.get('operation')
    if operation:
        operation_id = str(uuid.UUID(operation['id']))
        assert operation['application_id'] == app_id
        kind = operation['kind']
        target = operation.get('setup_action', 'connect_t3')
        assert kind in ('install', 'setup') and target in SETUP_ACTIONS
        if action['kind'] == 'submit_code':
            assert kind == 'setup' and target == 'sign_in_claude'
            guest(RUNNER, 'submit', app_id, operation_id, input=json.dumps({'code': action['code']}))
            return json.loads(guest(RUNNER, 'inspect', app_id).stdout)
        if action['kind'] != 'inspect':
            assert (action['kind'] == 'ensure_installed') == (kind == 'install')
        unit = 'homecloud-app-'+uuid.UUID(app_id).hex+'-'+uuid.UUID(operation_id).hex+'.service'
        previous = report.get('job')
        active = runtime.run('incus', 'exec', name, '--', 'systemctl', 'is-active', '--quiet', unit, timeout=10, check=False).returncode == 0
        if active:
            if not previous or previous['id'] != operation_id:
                report['job'] = {'id': operation_id, 'phase': 'running'}
                report['observation']['action'] = None
                report['observation']['operation_message'] = ''
            return report
        if action['kind'] == 'inspect':
            if not previous or previous['id'] != operation_id or previous['phase'] in ('running', 'waiting_for_user'):
                report['job'] = {'id': operation_id, 'phase': 'interrupted'}
                report['observation']['action'] = None
                if target == 'connect_t3' and report['observation']['setup'] not in ('finishing', 'ready'):
                    report['observation'].update(setup='interrupted', message='Setup interrupted. Request a new authorization link.')
            return report
        resume = previous and previous['id'] == operation_id and previous['phase'] == 'interrupted' and (kind == 'install' or (target == 'connect_t3' and report['observation']['setup'] == 'finishing'))
        if previous and previous['id'] == operation_id and not resume:
            return report
        # Install/start call is short. No host mutex or Incus exec stays open for login.
        runtime.run('incus', 'exec', name, '--', 'systemd-run', '--quiet', '--collect', '--unit='+unit,
                    '--uid=dev', '--property=UMask=0077', '--property=RuntimeMaxSec=1800',
                    '--property=TimeoutStopSec=10', '--property=KillMode=control-group',
                    *['--setenv='+x for x in env[1:]], RUNNER, 'job', app_id, operation_id, kind,
                    app['desired_version'], unit, target, timeout=10)
        report = json.loads(guest(RUNNER, 'inspect', app_id).stdout)
    return report
