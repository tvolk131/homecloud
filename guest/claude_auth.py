"""Version-pinned Claude CLI browser login, supervised in the guest.

OAuth and credential persistence belong to Claude. Only an allowlisted URL and
normalized status/account display metadata leave this module; the browser's
return code crosses a FIFO.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import stat
import struct
import subprocess
import termios
import time
from urllib.parse import urlsplit, parse_qs
from account_metadata import claude_account

BOOT = Path('/proc/sys/kernel/random/boot_id')
LOGIN_SECONDS = 600
ANSI = re.compile(r'\x1b\[[0-?]*[ -/]*[@-~]')
# OSC sequences end with BEL or ST. Hyperlinks contain their own hidden URL.
OSC = re.compile(r'\x1b\]([^\x07\x1b]*)(?:\x07|\x1b\\)')


def account_state():
    return account_info()['state']


def account_info():
    binary = shutil.which('claude')
    if not binary: return {'state': 'unavailable'}
    proc = subprocess.Popen([binary, 'auth', 'status'], stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        raw, _ = proc.communicate(timeout=15)
        if len(raw) > 65536: return {'state': 'unknown'}
        value = json.loads(raw)
        return claude_account(value, proc.returncode)
    except (ValueError, subprocess.TimeoutExpired):
        return {'state': 'unknown'}
    finally:
        if proc.poll() is None:
            try: os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError: pass
            proc.communicate()


def terminal_urls(output):
    targets = []
    for match in OSC.finditer(output):
        fields = match.group(1).split(';', 2)
        if len(fields) == 3 and fields[0] == '8' and fields[2]:
            targets.append(fields[2])
    # Remove complete OSC frames as units; stripping just ESC joins target and label.
    plain = OSC.sub('', output)
    # A partial frame is not a complete URL and may be finished by the next read.
    plain = re.sub(r'\x1b\][^\x07\x1b]*$', '', plain)
    plain = ANSI.sub('', plain)
    visible = re.findall(r'https://[^\s\x00-\x20\x7f-\x9f]+', plain)
    # Prefer visible URLs; a labeled hyperlink can supply its validated target.
    return visible + targets


def challenge(output, now):
    for url in terminal_urls(output):
        if any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in url): continue
        try:
            parsed = urlsplit(url)
            query = parse_qs(parsed.query)
        except ValueError:
            continue
        if (parsed.scheme == 'https' and parsed.netloc == 'claude.com'
                and parsed.path == '/cai/oauth/authorize'
                and query.get('response_type') and set(query['response_type']) == {'code'}
                and query.get('code') and set(query['code']) == {'true'}
                and query.get('state') and query.get('code_challenge')):
            return {'url': url, 'code': '', 'expires_at': int(now + LOGIN_SECONDS),
                    'setup_action': 'sign_in_claude', 'input_required': True}
    return None


def input_code(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9_.~#/+%=:?-]{8,2048}', value):
        raise RuntimeError('Paste the authorization code shown by Claude, then try again.')
    return value.encode('ascii')


def submit(directory, operation_id, value, read):
    code = input_code(value)
    state = read(directory/'job.json')
    if not (state and state['id'] == operation_id and state.get('setup_action') == 'sign_in_claude'
            and state['phase'] == 'waiting_for_user'
            and state['boot_id'] == BOOT.read_text().strip()
            and state.get('action', {}).get('input_required')
            and state['action']['expires_at'] > time.time()):
        raise RuntimeError('This Claude sign-in attempt is no longer waiting for a code. Start again.')
    fifo = directory/(operation_id+'.input')
    lock = (directory/'input.lock').open('a')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        fd = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
        try:
            info = os.fstat(fd)
            if not stat.S_ISFIFO(info.st_mode) or info.st_uid != os.getuid():
                raise RuntimeError('Claude sign-in input is unavailable. Start again.')
            if os.write(fd, code+b'\n') != len(code)+1:
                raise RuntimeError('Could not deliver the authorization code. Try again.')
        finally: os.close(fd)
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            latest = read(directory/'job.json')
            if latest and latest['id'] == operation_id and latest['phase'] != 'waiting_for_user': return
            time.sleep(0.02)
        raise RuntimeError('Code sent; confirmation is pending. Refresh the sign-in status.')
    except OSError:
        raise RuntimeError('Claude sign-in input is unavailable. Start again.') from None
    finally: lock.close()


def authorize(state, path, save):
    account = account_state()
    if account == 'signed_in': return
    if account not in ('signed_out', 'unavailable'):
        raise RuntimeError('Could not inspect the existing Claude account. Refresh its status before signing in.')
    binary = shutil.which('claude')
    if not binary: raise RuntimeError('Install Claude Code before signing in.')
    fifo = path.parent/(state['id']+'.input')
    os.mkfifo(fifo, 0o600)
    fd = os.open(fifo, os.O_RDWR | os.O_NONBLOCK | os.O_NOFOLLOW)
    master, slave = pty.openpty()
    # Keep the long OAuth URL on one line and disable echo of the returned code.
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 4096, 0, 0))
    attrs = termios.tcgetattr(slave); attrs[3] &= ~termios.ECHO; termios.tcsetattr(slave, termios.TCSANOW, attrs)
    env = dict(os.environ, BROWSER='/usr/bin/true', TERM='xterm-256color')
    proc = subprocess.Popen([binary, 'auth', 'login', '--claudeai'], stdin=slave, stdout=slave,
                            stderr=slave, env=env, start_new_session=True)
    os.close(slave)
    output = ''; action = None; delivered = False; pending = b''
    deadline = time.monotonic() + LOGIN_SECONDS
    try:
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master, fd], [], [], 0.2)
            if master in ready:
                try: chunk = os.read(master, 8192)
                except OSError: chunk = b''
                output = (output + chunk.decode(errors='replace'))[-65536:]
                if not action:
                    action = challenge(output, time.time())
                    plain = ANSI.sub('', output)
                    if action and 'Paste code' not in plain: action = None
                    if not action and 'Paste code' in plain and 'https://' in plain:
                        raise RuntimeError('Claude authorization page format was not recognized. Check the installed CLI version.')
                    if action:
                        state.update(phase='waiting_for_user', stage='awaiting_approval', action=action)
                        save(path, state)
            if fd in ready:
                pending += os.read(fd, 4096)
                if len(pending) > 4096: raise RuntimeError('Invalid Claude authorization response. Start again.')
            if b'\n' in pending and not delivered:
                code = input_code(pending.split(b'\n',1)[0].decode('ascii'))
                # Do not inject input before the pinned CLI is expecting its auth code.
                if 'Paste code' not in ANSI.sub('', output):
                    raise RuntimeError('Claude is not ready for the authorization code. Start again.')
                delivered = True; pending = b''; output = ''
                state.update(phase='running', stage='verifying'); state.pop('action', None); save(path, state)
                os.write(master, code+b'\r'); code = b''
            if proc.poll() is not None:
                if proc.returncode == 0 and account_state() == 'signed_in': return
                raise RuntimeError('Claude sign-in did not finish. Start a new sign-in attempt to try again.')
            if action and time.time() >= action['expires_at']: break
        raise RuntimeError('Claude sign-in attempt expired. Start again for a new link.')
    finally:
        if proc.poll() is None:
            try: os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError: pass
            try: proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                try: os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError: pass
                proc.wait()
        os.close(master); os.close(fd); fifo.unlink(missing_ok=True)
