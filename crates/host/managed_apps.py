"""Fixed host-side managed-application hooks; guest jobs are supervised by systemd."""
import json
from pathlib import Path
import subprocess
import uuid
import managed_host

GUEST_DIR = '/usr/local/libexec/homecloud'


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

    script = GUEST_DIR + '/managed-app.py'
    # Upgrade the fixed runner independently of the application installation.
    # Inspecting an older VM must discover new account capabilities without
    # reinstalling T3 or restarting a user's active sessions.
    sources = {file: Path(__file__).with_name(file).read_text()
               for file in ['managed-app.py', 'setup-t3.sh', 'setup-providers.py', 'codex_auth.py', 'claude_auth.py', 'account_metadata.py']}
    writer = """import json,os
from pathlib import Path
root=Path('/usr/local/libexec/homecloud'); root.mkdir(parents=True,exist_ok=True,mode=0o755)
for name,source in json.load(__import__('sys').stdin).items():
 p=root/name
 if p.is_file() and not p.is_symlink() and p.read_text()==source: continue
 t=str(p)+'.new'; f=os.open(t,os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o644)
 with os.fdopen(f,'w') as stream: stream.write(source)
 os.replace(t,p)
"""
    runtime.run('incus', 'exec', name, '--', 'python3', '-c', writer, input=json.dumps(sources), timeout=10)
    report = json.loads(guest('python3', script, 'inspect', app_id).stdout)
    operation = action.get('operation')
    if operation:
        operation_id = str(uuid.UUID(operation['id']))
        assert operation['application_id'] == app_id
        kind = operation['kind']
        target = operation.get('setup_action', 'connect_t3')
        assert kind in ('install', 'setup') and target in ('connect_t3', 'sign_in_codex', 'sign_in_claude')
        if action['kind'] == 'submit_code':
            assert kind == 'setup' and target == 'sign_in_claude'
            guest('python3', script, 'submit', app_id, operation_id, input=json.dumps({'code': action['code']}))
            return json.loads(guest('python3', script, 'inspect', app_id).stdout)
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
                    *['--setenv='+x for x in env[1:]], 'python3', script, 'job', app_id, operation_id, kind,
                    app['desired_version'], unit, target, timeout=10)
        report = json.loads(guest('python3', script, 'inspect', app_id).stdout)
    return report
