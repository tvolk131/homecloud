"""GitHub CLI device-flow sign-in and sign-out, supervised in the guest.

gh owns OAuth and its credential file. Homecloud reads only the active login
through gh's configuration interface (never the token) plus the public device
challenge. Git is pointed at gh's credential helper before any token exists.
"""
import json
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import signal
import subprocess
import time
from account_metadata import display_text

HOST = 'github.com'
DEVICE_URL = 'https://github.com/login/device'
LOGIN_SECONDS = 600  # GitHub device codes last 15 minutes; Homecloud waits less.
MAX_OUTPUT = 65536
# Includes Enterprise Managed User handles such as octocat_acme.
LOGIN = re.compile(r'[A-Za-z0-9][A-Za-z0-9_-]{0,63}')
CODE = re.compile(r'one-time code: ([A-Z0-9]{4}-[A-Z0-9]{4})\s')
URL = re.compile(r'Open this URL to continue in your web browser: (\S+)\s')
# Tokens in the environment would override, and hide, the stored sign-in.
IGNORED_ENV = ('GH_TOKEN', 'GITHUB_TOKEN', 'GH_ENTERPRISE_TOKEN', 'GITHUB_ENTERPRISE_TOKEN', 'GH_HOST')
FAILURE = 'GitHub sign-in did not finish. The code may have expired or been cancelled. Start again for a new code.'
EXPIRED = 'GitHub sign-in attempt expired. Start again for a new code.'


class ProtocolError(RuntimeError):
    pass


def identity_path():
    return Path.home()/'.local/state/homecloud/github-identity.json'


def environment():
    env = {k: v for k, v in os.environ.items() if k not in IGNORED_ENV}
    env.update(GH_PROMPT_DISABLED='1', GH_NO_UPDATE_NOTIFIER='1', GH_NO_EXTENSION_UPDATE_NOTIFIER='1',
               GH_SPINNER_DISABLED='1', NO_COLOR='1')
    return env


def stop(proc):
    if proc.poll() is None:
        try: os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError: pass
        try: proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            try: os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError: pass
            proc.wait()


def run(args, timeout=15, with_stderr=False):
    """Run a fixed command; output is bounded and never forwarded verbatim."""
    proc = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            env=environment(), cwd=str(Path.home()), start_new_session=True)
    try:
        out, error = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        stop(proc); proc.communicate()
        raise ProtocolError('The GitHub CLI did not respond. Try again.') from None
    result = (proc.returncode, out[:MAX_OUTPUT])
    return (*result, error[:MAX_OUTPUT]) if with_stderr else result


def gh(*args, timeout=15, with_stderr=False):
    binary = shutil.which('gh')
    if not binary: raise ProtocolError('Install the GitHub CLI before signing in.')
    return run([binary, *args], timeout=timeout, with_stderr=with_stderr)


def account_info():
    """Local-only: gh's saved active login, not a network validation of its token."""
    if not shutil.which('gh'): return {'state': 'unavailable'}
    try:
        code, out, error = gh('config', 'get', '--host', HOST, 'user', with_stderr=True)
    except (OSError, RuntimeError):
        return {'state': 'unknown'}
    login = out.decode(errors='replace').strip()
    if code == 0 and LOGIN.fullmatch(login):
        return {'state': 'signed_in', 'username': login}
    if code == 1 and not login and error.strip() == b'could not find key "user"':
        return {'state': 'signed_out'}
    return {'state': 'unknown'}


def challenge(output, now):
    """Parse complete lines only, so a partially received URL is never judged."""
    complete = output[:output.rfind('\n') + 1]
    code, url = CODE.search(complete), URL.search(complete)
    if not url: return None
    if url.group(1) != DEVICE_URL:
        # Never turn an arbitrary guest-supplied URL into a credential page.
        raise ProtocolError('Unexpected GitHub authorization page. Check the installed GitHub CLI version.')
    # gh prints the code line before the URL line; a URL alone means a changed format.
    if not code:
        raise ProtocolError('GitHub sign-in code was not recognized. Check the installed GitHub CLI version.')
    return {'url': DEVICE_URL, 'code': code.group(1), 'expires_at': int(now + LOGIN_SECONDS),
            'setup_action': 'sign_in_github'}


def git_value(git, key):
    code, out = run([git, 'config', '--get', key])
    return out.decode(errors='replace').strip() if code == 0 else None


def configure_git():
    git = shutil.which('git')
    binary = shutil.which('gh')
    if not git or not binary: raise ProtocolError('Install Git and GitHub CLI before signing in.')
    for hostname in (HOST, 'gist.github.com'):
        code, output = run([git, 'config', '--get-urlmatch', 'credential.helper', 'https://'+hostname])
        if code not in (0, 1): raise ProtocolError('Could not inspect Git credential helpers. Review Git configuration before signing in.')
        for helper in output.decode(errors='replace').splitlines():
            if not helper.strip(): continue
            try: command = shlex.split(helper[1:]) if helper.startswith('!') else []
            except ValueError: command = []
            if command not in ([binary, 'auth', 'git-credential'], ['gh', 'auth', 'git-credential']):
                raise ProtocolError('Git already uses another credential helper for GitHub. Review that helper before connecting this VM.')
    if gh('auth', 'setup-git', '--hostname', HOST, '--force', timeout=30)[0]:
        raise ProtocolError('Could not configure Git to use the GitHub CLI. Try again.')


def configure_identity(login):
    """Fill an unset Git author from the account; never overwrite the owner's own values.

    Values Homecloud wrote earlier (still unchanged) follow a later account change.
    Best effort: a network or profile problem leaves Git configuration untouched.
    """
    git = shutil.which('git')
    if not git: return
    try:
        code, out = gh('api', 'user', timeout=20)
        profile = json.loads(out) if code == 0 else None
    except (OSError, RuntimeError, ValueError):
        return
    if (not isinstance(profile, dict) or profile.get('login') != login
            or not isinstance(profile.get('id'), int) or isinstance(profile.get('id'), bool)):
        return
    desired = {'user.name': display_text(profile.get('name'), 120) or login,
               # GitHub's private address avoids exposing, or being blocked by, a hidden email.
               'user.email': '%d+%s@users.noreply.github.com' % (profile['id'], login)}
    marker = identity_path()
    try: previous = json.loads(marker.read_text())
    except (OSError, ValueError): previous = {}
    if not isinstance(previous, dict): previous = {}
    written = {key: value for key, value in previous.items() if key in desired and isinstance(value, str)}
    for key, value in desired.items():
        try:
            current = git_value(git, key)
            if current is None or current == previous.get(key):
                if run([git, 'config', '--global', key, value])[0] == 0: written[key] = value
            else: written.pop(key, None)
        except (OSError, RuntimeError):
            continue
    marker.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = marker.with_name(marker.name + '.new')
    temporary.write_text(json.dumps(written))
    temporary.replace(marker)


def authorize(state, path, save):
    account = account_info()
    if account['state'] not in ('signed_in', 'signed_out'):
        raise ProtocolError('Could not inspect the existing GitHub sign-in. Refresh its status before signing in.')
    configure_git()
    if account['state'] == 'signed_in':
        configure_identity(account['username'])
        return
    binary = shutil.which('gh')
    # Without a terminal gh prints the device code and URL, never prompting.
    proc = subprocess.Popen([binary, 'auth', 'login', '--hostname', HOST, '--web', '--git-protocol', 'https',
                             '--skip-ssh-key', '--insecure-storage'],
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            env=environment(), cwd=str(Path.home()), start_new_session=True)
    output = ''; action = None
    deadline = time.monotonic() + LOGIN_SECONDS
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0: raise ProtocolError(EXPIRED)
            if not select.select([proc.stdout], [], [], min(remaining, 1))[0]: continue
            chunk = os.read(proc.stdout.fileno(), 8192)
            if not chunk: break
            output = (output + chunk.decode(errors='replace'))[-MAX_OUTPUT:]
            if not action:
                action = challenge(output, time.time())
                if action:
                    state.update(phase='waiting_for_user', stage='awaiting_approval', action=action)
                    save(path, state)
        try: returncode = proc.wait(timeout=max(1, deadline - time.monotonic()))
        except subprocess.TimeoutExpired: raise ProtocolError(EXPIRED) from None
        # Upstream output can contain identities; only authored messages leave here.
        if returncode != 0: raise ProtocolError(FAILURE)
    finally:
        stop(proc); proc.stdout.close()
    state.update(phase='running', stage='verifying'); state.pop('action', None); save(path, state)
    info = account_info()
    if info['state'] != 'signed_in':
        raise ProtocolError('Could not confirm the saved GitHub sign-in. Try again.')
    configure_identity(info['username'])


def sign_out(state, path, save):
    """Remove every saved github.com account from this VM. GitHub keeps the token valid."""
    for _ in range(8):
        info = account_info()
        if info['state'] == 'signed_out': return
        if info['state'] != 'signed_in':
            raise ProtocolError('Could not inspect the GitHub sign-in. Refresh its status and try again.')
        if gh('auth', 'logout', '--hostname', HOST, '--user', info['username'], timeout=30)[0]:
            raise ProtocolError('Could not remove GitHub access from this VM. Try again.')
    raise ProtocolError('Could not remove every GitHub account from this VM. Try again.')
