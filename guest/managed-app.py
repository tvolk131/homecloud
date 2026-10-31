#!/usr/bin/python3
"""Guest-side supervised jobs. Runs as dev; never listens on a network socket.

Only the compiled-in T3 adapter is supported. Private job state is durable;
OAuth device secrets remain owned by T3's CLI process and secret store.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import shutil
import subprocess
import sys
import time
import tempfile
from account_metadata import display_text
import codex_auth
import claude_auth
import github_auth
import uuid

BASE = Path.home() / '.local/state/homecloud/apps'
T3 = Path.home() / '.local/bin/t3'
BOOT = Path('/proc/sys/kernel/random/boot_id')
ANSI = re.compile(r'\x1b\[[0-?]*[ -/]*[@-~]')
# binary, component name, minimum supported version, account adapter.
PROVIDERS = [('codex', 'Codex CLI', (0, 159, 0), codex_auth),
             ('claude', 'Claude Code', (2, 1, 280), claude_auth),
             ('gh', 'GitHub CLI', (2, 40, 0), github_auth)]
# Compiled-in account actions: setup action -> (binary, component, account state on success).
ACCOUNT_ACTIONS = {'sign_in_codex': ('codex', 'Codex CLI', 'signed_in'),
                   'sign_in_claude': ('claude', 'Claude Code', 'signed_in'),
                   'sign_in_github': ('gh', 'GitHub CLI', 'signed_in'),
                   'sign_out_github': ('gh', 'GitHub CLI', 'signed_out')}
SETUP_ACTIONS = ('connect_t3', *ACCOUNT_ACTIONS)


def interrupted_message(setup_action):
    if setup_action == 'sign_out_github': return 'Removing GitHub access was interrupted. Try again.'
    if setup_action in ACCOUNT_ACTIONS: return 'Provider sign-in was interrupted. Request a new sign-in link.'
    return 'Setup interrupted. Request a new authorization link.'


def success_message(setup_action, component):
    if setup_action == 'sign_in_github': return 'GitHub sign-in saved in this VM. Git and gh now use this account.'
    if setup_action == 'sign_out_github':
        return 'GitHub access removed from this VM. Its token stays valid until revoked in GitHub settings.'
    return component+' sign-in saved in this VM. Refresh provider checks in T3 Code.'


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, name = tempfile.mkstemp(prefix='.job-', dir=path.parent)
    temp = Path(name)
    try:
        with os.fdopen(fd, 'w') as stream:
            json.dump(value, stream)
            stream.flush(); os.fsync(stream.fileno())
        temp.replace(path)
    finally:
        temp.unlink(missing_ok=True)


def read(path):
    return json.loads(path.read_text()) if path.exists() else None


def command(args, timeout=20):
    proc = subprocess.Popen([str(x) for x in args], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, start_new_session=True)
    try:
        out, _ = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL); proc.communicate()
        raise RuntimeError('Application command timed out') from None
    if proc.returncode: raise RuntimeError('Application command failed')
    return out


def t3_status():
    if not T3.exists(): return None
    try: return json.loads(command([T3, 'connect', 'status', '--json'], timeout=15))
    except (RuntimeError, ValueError): return None


def provider_components(directory, completed=None):
    components = []
    for binary, label, minimum, adapter in PROVIDERS:
        path = shutil.which(binary)
        found = None
        if path:
            file = Path(path); stamp = [str(file.resolve()), file.stat().st_mtime_ns, file.stat().st_size]
            cache = read(directory/(binary+'-version.json'))
            if cache and cache['stamp'] == stamp:
                found = cache['version']
            else:
                try:
                    output = command([path, '--version'], timeout=30)
                    match = re.search(r'(?<![0-9])(\d+\.\d+\.\d+)', output)
                    if match:
                        found = match.group(1)
                        save(directory/(binary+'-version.json'), {'stamp': stamp, 'version': found})
                except RuntimeError: pass
        ready = found is not None and tuple(map(int, found.split('.'))) >= minimum
        info = {'state': 'unavailable'}
        checked_at = time.time()
        cache = read(directory/(binary+'-account.json'))
        if ready:
            if cache and isinstance(cache.get('metadata'), dict) and cache['observed_at'] >= (completed or {}).get(binary, 0) and 0 <= time.time() - cache['observed_at'] < 10:
                info = cache['metadata']; checked_at = cache['observed_at']
            else:
                try: info = adapter.account_info()
                except (OSError, RuntimeError): info = {'state': 'unknown'}
                save(directory/(binary+'-account.json'), {'account': info['state'], 'metadata': info, 'observed_at': checked_at})
        metadata = None
        if info['state'] == 'signed_in':
            metadata = {'name': label, 'email': info.get('email'), 'account_id': info.get('account_id'),
                        'username': info.get('username'), 'plan': info.get('plan'),
                        'authentication': info.get('authentication'), 'observed_at': int(checked_at)}
        components.append({'name': label, 'version': found, 'ready': ready, 'account': info['state'], 'metadata': metadata})
    return components


def inspect(app_id):
    directory = BASE / app_id
    state = read(directory / 'job.json')
    if state and state['phase'] in ('running', 'waiting_for_user') and state['boot_id'] != BOOT.read_text().strip():
        state['phase'] = 'interrupted'; state.pop('action', None)
        state['message'] = interrupted_message(state.get('setup_action'))
        save(directory / 'job.json', state)
    # A stopped/failed transient unit cannot leave an immortal waiting operation.
    if state and state['phase'] in ('running', 'waiting_for_user'):
        result = subprocess.run(['systemctl', 'is-active', '--quiet', state['unit']], capture_output=True)
        if result.returncode:
            state['phase'] = 'interrupted'; state.pop('action', None)
            state['message'] = interrupted_message(state.get('setup_action'))
            save(directory / 'job.json', state)
    version = None
    if T3.exists():
        stamp = [T3.stat().st_mtime_ns, T3.stat().st_size]
        cache = read(directory / 'version.json')
        if cache and cache['stamp'] == stamp:
            version = cache['version']
        else:
            try:
                raw = command([T3, '--version'], timeout=30)
                match = re.search(r'(?<![0-9])(\d+\.\d+\.\d+(?:[-+][\w.-]+)?)\b', raw)
                if match:
                    version = match.group(1)
                    save(directory / 'version.json', {'stamp': stamp, 'version': version})
            except RuntimeError: pass
    service = subprocess.run(['systemctl', '--user', 'is-active', 't3code.service'], capture_output=True, text=True)
    status = t3_status()
    setup = 'unknown' if version and status is None else 'required'
    message = 'Install T3 Code to get started' if not version else 'Connect your T3 account'
    if status:
        if status.get('revoked'):
            message = 'T3 account connection was revoked. Connect again to authorize this VM.'
        elif status.get('authenticated') and status.get('desired') and status.get('linked'):
            setup = 'ready'
            reachable = status.get('reachability') or {}
            message = 'Linked to T3. Sign into the same T3 account on another device.' if reachable.get('status') == 'reachable' else 'Account connected; remote connectivity is not currently confirmed.'
        elif status.get('authenticated'):
            setup = 'finishing'; message = 'Account authorization saved; finishing connection'
    if version and status is None:
        message = 'Could not inspect the T3 connection; retrying'
    completed = {}
    if state and state['phase'] == 'succeeded':
        account_action = ACCOUNT_ACTIONS.get(state.get('setup_action'))
        if account_action: completed[account_action[0]] = state.get('completed_at', 0)
    components = provider_components(directory, completed)
    accounts = [c.pop('metadata') for c in components if c.get('metadata') is not None]
    for component in components: component.pop('metadata', None)
    if status and status.get('authenticated') and display_text(status.get('cloudUserId'), 128):
        accounts.insert(0, {'name': 'T3', 'account_id': display_text(status['cloudUserId'], 128),
                            'observed_at': int(time.time()), 'email': None, 'plan': None, 'authentication': None})
    action = None
    operation_message = ''
    if state:
        target = state.get('setup_action', 'connect_t3')
        _, provider, expected = ACCOUNT_ACTIONS.get(target, (None, None, None))
        provider_setup = provider is not None and state['kind'] == 'setup'
        confirmed = (any(c['name'] == provider and c['account'] == expected for c in components)
                     if provider_setup else setup == 'ready')
        # Recovery only repairs the matching setup target. T3 being paired says
        # nothing about whether Codex credentials were ever saved.
        if confirmed and state['kind'] == 'setup' and state['phase'] in ('interrupted', 'failed'):
            state['phase'] = 'succeeded'; state.pop('action', None); save(directory / 'job.json', state)
        if state['phase'] == 'waiting_for_user':
            candidate = state.get('action')
            if candidate and candidate['expires_at'] > time.time():
                action = dict(candidate, setup_action=target)
                if not provider_setup: setup = 'waiting_for_user'; message = 'Approve this VM in your browser'
        elif state['phase'] == 'running':
            if not provider_setup:
                setup = 'in_progress'; message = 'Installing T3 Code' if state['kind'] == 'install' else 'Finishing T3 connection'
        elif state['phase'] in ('failed', 'interrupted'):
            operation_message = state.get('message', 'Setup interrupted. Request a new sign-in link.')
            if not provider_setup and setup not in ('ready', 'finishing'):
                setup = state['phase']; message = operation_message
        if provider_setup and state['phase'] == 'succeeded':
            operation_message = success_message(target, provider)
    return {'observation': {'accounts': accounts, 'installed_version': version, 'service': 'running' if service.returncode == 0 else 'stopped',
                            'setup': setup, 'components': components, 'message': message, 'operation_message': operation_message,
                            'observed_at': int(time.time()), 'action': action,
                            'operation_id': state['id'] if state else None, 'operation_stage': state.get('stage','preparing') if state else ''},
            'job': {'id': state['id'], 'phase': state['phase']} if state else None}


def parse_challenge(text, now):
    text = ANSI.sub('', text).replace('\r', '')
    if 'Headless authorization' not in text: return None
    match = re.search(r'Open this URL on a device with a browser:\s*(https://[^\s]+)', text)
    code = re.search(r'Confirm this code when asked:\s*([^\s]+)', text)
    expiry = re.search(r'expires in (\d+) min', text)
    if not (match and code and expiry): return None
    # CLI rounds minutes; expire conservatively rather than advertise stale codes.
    ttl = max(30, min(1800, int(expiry.group(1)) * 60 - 30))
    return {'url': match.group(1), 'code': code.group(1), 'expires_at': int(now + ttl)}


def authorize(state, path):
    master, slave = pty.openpty()
    proc = subprocess.Popen([str(T3), 'connect', 'link', '--headless'], stdin=slave, stdout=slave,
                            stderr=slave, start_new_session=True)
    os.close(slave)
    output = ''; answered = False; challenge = None
    deadline = time.monotonic() + 1800
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.2)[0]:
                try: chunk = os.read(master, 8192)
                except OSError: chunk = b''
                if chunk:
                    output = (output + chunk.decode(errors='replace'))[-65536:]
                    plain = ANSI.sub('', output)
                    # Only approve T3's known, version-pinned relay installation prompt.
                    if not answered and 'Download and install version' in plain and 'T3 relay client' in plain:
                        os.write(master, b'y\r'); answered = True
                    if not challenge:
                        challenge = parse_challenge(output, time.time())
                        if challenge:
                            state.update(phase='waiting_for_user', stage='awaiting_approval', action=challenge)
                            save(path, state)
            if proc.poll() is not None:
                if proc.returncode: raise RuntimeError('Authorization did not finish. Request a new link to try again.')
                return
            if challenge and time.time() >= challenge['expires_at']:
                raise RuntimeError('Authorization link expired. Request a new link to continue.')
        raise RuntimeError('T3 setup timed out. Try connecting again.')
    finally:
        if proc.poll() is None:
            os.killpg(proc.pid, signal.SIGTERM)
            try: proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL); proc.wait()
        os.close(master)


def job(app_id, operation_id, kind, version, unit, setup_action='connect_t3'):
    directory = BASE / app_id; directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    lock = (directory / 'worker.lock').open('a'); fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    path = directory / 'job.json'
    state = {'id': operation_id, 'kind': kind, 'phase': 'running', 'unit': unit, 'boot_id': BOOT.read_text().strip(), 'setup_action': setup_action, 'stage': 'preparing'}
    save(path, state)
    try:
        if kind == 'install':
            if not Path('/usr/lib/x86_64-linux-gnu/libatomic.so.1').exists():
                command(['sudo', '-n', 'apt-get', 'install', '-y', 'libatomic1'], timeout=300)
            command(['sh', '/usr/local/libexec/homecloud/setup-t3.sh', version], timeout=1200)
        elif setup_action == 'sign_in_claude':
            claude_auth.authorize(state, path, save)
            (directory/'claude-account.json').unlink(missing_ok=True)
        elif setup_action == 'sign_in_codex':
            codex_auth.authorize(state, path, save)
            (directory/'codex-account.json').unlink(missing_ok=True)
        elif setup_action in ('sign_in_github', 'sign_out_github'):
            try:
                (github_auth.authorize if setup_action == 'sign_in_github' else github_auth.sign_out)(state, path, save)
            finally:
                (directory/'gh-account.json').unlink(missing_ok=True)
        else:
            status = t3_status()
            if not (status and status.get('authenticated') and status.get('desired') and status.get('linked') and not status.get('revoked')):
                authorize(state, path)
                state.update(phase='running', stage='verifying'); state.pop('action', None); save(path, state)
                command([T3, 'service', 'restart'], timeout=60)
                # Saved credentials survive network trouble; pairing need not be repeated.
                status = t3_status()
                if not (status and status.get('authenticated') and status.get('desired')):
                    raise RuntimeError('Could not confirm saved account connection. Inspect T3 and try again.')
        state.update(phase='succeeded', stage='complete', completed_at=time.time()); state.pop('action', None)
    except Exception as error:
        # Error strings are deliberately authored here, never raw CLI output.
        state.update(phase='failed', message=(str(error) if isinstance(error, (RuntimeError, codex_auth.ProtocolError)) else 'Application setup failed. Please try again.')); state.pop('action', None)
    save(path, state)


def main():
    os.umask(0o077)
    assert os.getuid() != 0, 'Run guest applications as dev'
    mode, app_id, *args = sys.argv[1:]; app_id = str(uuid.UUID(app_id))
    if mode == 'inspect': print(json.dumps(inspect(app_id)))
    elif mode == 'submit':
        operation_id, = args
        value = json.load(sys.stdin)
        claude_auth.submit(BASE/app_id, str(uuid.UUID(operation_id)), value.get('code'), read)
    elif mode == 'job':
        operation_id, kind, version, unit, *target = args
        setup_action = target[0] if target else 'connect_t3'
        assert setup_action in SETUP_ACTIONS
        assert kind in ('install', 'setup') and version == '0.1.2'
        job(app_id, str(uuid.UUID(operation_id)), kind, version, unit, setup_action)
    else: raise ValueError('Unsupported guest application action')

if __name__ == '__main__': main()
