"""Codex-owned OAuth over its versioned stdio app-server protocol.

Only normalized account state, display metadata and a public
device challenge leave this adapter.
No model requests, external-token auth, or credential-file reads are needed.
"""
import json
import os
import re
import select
import shutil
import signal
import subprocess
import time
import uuid
from account_metadata import codex_account

LOGIN_SECONDS = 600  # Homecloud's waiting limit, not an upstream expiry guarantee.
MAX_FRAME = 1024 * 1024
FAILURE = ('Codex sign-in did not finish. Check internet access and device-code login in '
           'ChatGPT Settings > Security (or ask your workspace administrator), then retry.')


class ProtocolError(RuntimeError):
    pass


class Session:
    def __init__(self):
        binary = shutil.which('codex')
        if not binary:
            raise ProtocolError('Install the Codex CLI before signing in.')
        self.proc = subprocess.Popen([binary, 'app-server', '--listen', 'stdio://'],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, start_new_session=True)
        self.buffer = b''
        self.notifications = []
        self.sequence = 0
        self.login_id = None

    def __enter__(self):
        try:
            self.request('initialize', {'clientInfo': {'name': 'homecloud', 'version': '0.1.0'},
                                        'capabilities': {'experimentalApi': False}}, timeout=15)
            self.send({'method': 'initialized'})
            return self
        except Exception:
            self.close()
            raise

    def __exit__(self, *exc):
        self.close()

    def send(self, value):
        try:
            self.proc.stdin.write(json.dumps(value).encode() + b'\n')
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError):
            raise ProtocolError('Codex sign-in process stopped. Try again.') from None

    def receive(self, deadline):
        while b'\n' not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.proc.stdout], [], [], remaining)[0]:
                raise ProtocolError('Codex sign-in timed out. Request a new sign-in link.')
            chunk = os.read(self.proc.stdout.fileno(), 8192)
            if not chunk:
                raise ProtocolError('Codex sign-in process stopped. Try again.')
            self.buffer += chunk
            if len(self.buffer) > MAX_FRAME:
                raise ProtocolError('Unexpected Codex login response. Check the installed CLI version.')
        raw, self.buffer = self.buffer.split(b'\n', 1)
        try:
            value = json.loads(raw)
            if not isinstance(value, dict): raise ValueError()
            return value
        except (ValueError, UnicodeError):
            raise ProtocolError('Unexpected Codex login response. Check the installed CLI version.') from None

    def request(self, method, params, timeout=15):
        self.sequence += 1
        request_id = self.sequence
        self.send({'id': request_id, 'method': method, 'params': params})
        deadline = time.monotonic() + timeout
        while True:
            value = self.receive(deadline)
            if value.get('id') == request_id and 'method' not in value:
                if 'error' in value:
                    # Upstream error text can contain identities/URLs. Never persist it.
                    raise ProtocolError(FAILURE)
                result = value.get('result')
                if not isinstance(result, dict): raise ProtocolError('Unexpected Codex login response.')
                return result
            self.accept_notification(value)

    def accept_notification(self, value):
        if 'id' in value and 'method' in value:
            self.send({'id': value['id'], 'error': {'code': -32601, 'message': 'Unsupported method'}})
        elif value.get('method') == 'account/login/completed':
            if len(self.notifications) >= 32: raise ProtocolError('Unexpected Codex login response.')
            self.notifications.append(value)

    def account(self):
        return codex_account(self.request('account/read', {'refreshToken': False}))['state']

    def close(self):
        try:
            if self.login_id and self.proc.poll() is None:
                self.request('account/login/cancel', {'loginId': self.login_id}, timeout=2)
        except (OSError, RuntimeError):
            pass
        finally:
            if self.proc.poll() is None:
                try: os.killpg(self.proc.pid, signal.SIGTERM)
                except ProcessLookupError: pass
                try: self.proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    try: os.killpg(self.proc.pid, signal.SIGKILL)
                    except ProcessLookupError: pass
                    self.proc.wait()
            self.proc.stdin.close(); self.proc.stdout.close()


def account_state():
    if not shutil.which('codex'): return 'unavailable'
    try:
        with Session() as session: return session.account()
    except (OSError, RuntimeError):
        return 'unknown'


def account_info():
    if not shutil.which('codex'): return {'state': 'unavailable'}
    try:
        with Session() as session:
            return codex_account(session.request('account/read', {'refreshToken': False}))
    except (OSError, RuntimeError):
        return {'state': 'unknown'}


def challenge(result, now):
    # Never turn an arbitrary guest-supplied URL into a credential collection page.
    if (result.get('type') != 'chatgptDeviceCode'
            or result.get('verificationUrl') != 'https://auth.openai.com/codex/device'
            or not isinstance(result.get('userCode'), str)
            or not re.fullmatch(r'[A-Za-z0-9-]{4,32}', result['userCode'])):
        raise ProtocolError('Unexpected Codex authorization page. Check the installed CLI version.')
    try: login_id = str(uuid.UUID(result['loginId']))
    except (KeyError, ValueError, TypeError, AttributeError):
        raise ProtocolError('Unexpected Codex login response.') from None
    return login_id, {'url': result['verificationUrl'], 'code': result['userCode'],
                      'expires_at': int(now + LOGIN_SECONDS), 'setup_action': 'sign_in_codex'}


def authorize(state, path, save):
    with Session() as session:
        account = session.account()
        if account == 'signed_in': return  # Never overwrite an existing account.
        if account != 'signed_out': raise ProtocolError('Could not inspect the existing Codex account. Refresh its status before signing in.')
        result = session.request('account/login/start', {'type': 'chatgptDeviceCode'}, timeout=45)
        session.login_id, action = challenge(result, time.time())
        state.update(phase='waiting_for_user', stage='awaiting_approval', action=action, message='Approve Codex sign-in in your browser.')
        save(path, state)
        deadline = time.monotonic() + LOGIN_SECONDS
        while True:
            if not session.notifications:
                session.accept_notification(session.receive(deadline))
            if not session.notifications: continue
            value = session.notifications.pop(0).get('params', {})
            if value.get('loginId') != session.login_id: continue
            if value.get('success') is not True: raise ProtocolError(FAILURE)
            state.update(phase='running', stage='verifying'); state.pop('action', None); save(path, state)
            # Check the credential store through Codex; a notification alone isn't proof.
            if session.account() != 'signed_in':
                raise ProtocolError('Could not confirm saved Codex sign-in. Try again.')
            session.login_id = None
            return
