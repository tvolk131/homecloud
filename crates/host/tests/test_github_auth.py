"""Exercise GitHub CLI sign-in/out against a disposable fake gh and a real, isolated git."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).parents[3] / 'guest'))
import github_auth as auth

# State lives in $FAKE_GH/accounts.json (active first); every call is logged.
FAKE = r'''#!/usr/bin/env python3
import json, os, sys, time
root = os.environ['FAKE_GH']; mode = open(os.path.join(root, 'mode')).read().strip()
state_path = os.path.join(root, 'accounts.json')
accounts = json.load(open(state_path)) if os.path.exists(state_path) else []
args = sys.argv[1:]
with open(os.path.join(root, 'calls'), 'a') as log:
    log.write(json.dumps({'args': args, 'token_env': any(k in os.environ for k in ('GH_TOKEN', 'GITHUB_TOKEN'))}) + '\n')
def out(text):
    sys.stdout.write(text); sys.stdout.flush()
if args[:2] == ['config', 'get']:
    if mode == 'config_error': sys.stderr.write('failed to load config: invalid YAML\n'); sys.exit(1)
    if mode == 'garbled_config': out('not a login!\n'); sys.exit(0)
    if accounts: out(accounts[0] + '\n'); sys.exit(0)
    sys.stderr.write('could not find key "user"\n'); sys.exit(1)
if args[:2] == ['auth', 'setup-git']:
    sys.exit(3 if mode == 'setup_git_fails' else 0)
if args[:2] == ['auth', 'login']:
    out('\n! Failed to copy one-time code to clipboard\n  No clipboard utilities available.\n')
    if mode == 'no_code':
        out('Open this URL to continue in your web browser: https://github.com/login/device\n'); time.sleep(30); sys.exit(1)
    out('! First copy your one-time code: AB12-CD34\n')
    url = 'https://evil.invalid/capture' if mode == 'wrong_url' else 'https://github.com/login/device'
    if mode == 'split_url':
        # A partially received line must not be judged as a different URL.
        out('Open this URL to continue in your web browser: https://github.com/lo'); time.sleep(0.3)
        out('gin/device\n')
    else:
        out('Open this URL to continue in your web browser: ' + url + '\n')
    if mode == 'hang': time.sleep(30); sys.exit(1)
    time.sleep(0.2)
    if mode == 'denied':
        sys.stderr.write('PRIVATE-IDENTITY failed https://secret.invalid\n'); sys.exit(1)
    login_as = open(os.path.join(root, 'login_as')).read().strip() if os.path.exists(os.path.join(root, 'login_as')) else 'octo-fixture'
    if mode != 'unsaved':
        json.dump([login_as] + [a for a in accounts if a != login_as], open(state_path, 'w'))
    out('Authentication complete. Logged in as ' + login_as + '\n'); sys.exit(0)
if args[:2] == ['auth', 'logout']:
    user = args[args.index('--user') + 1]
    if mode == 'logout_fails' or user not in accounts: sys.exit(1)
    json.dump([a for a in accounts if a != user], open(state_path, 'w')); sys.exit(0)
if args == ['api', 'user']:
    if mode == 'api_fails': sys.exit(1)
    login = accounts[0] if accounts else None
    out(json.dumps({'login': login, 'id': {'octo-fixture': 4242, 'new-fixture': 99}.get(login, 7), 'name': None if mode == 'no_name' else 'Octo Fixture'}))
    sys.exit(0)
sys.exit(64)
'''


@unittest.skipUnless(shutil.which('git'), 'Requires git for isolated configuration tests')
class GitHubAuthTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); root = Path(self.tmp.name)
        self.root = root; self.home = root/'home'; self.home.mkdir()
        self.binary = root/'bin/gh'; self.binary.parent.mkdir(); self.binary.write_text(FAKE); self.binary.chmod(0o700)
        git = shutil.which('git')
        self.paths = {'gh': str(self.binary), 'git': git}
        env = {'HOME': str(self.home), 'FAKE_GH': str(root), 'GIT_CONFIG_NOSYSTEM': '1',
               'GH_TOKEN': 'PRIVATE-ENV-TOKEN', 'PATH': os.environ.get('PATH', '')}
        self.patches = [patch.dict(os.environ, env, clear=True),
                        patch.object(auth.shutil, 'which', side_effect=lambda name: self.paths.get(name))]
        for p in self.patches: p.start()
        self.mode('success')

    def tearDown(self):
        for p in reversed(self.patches): p.stop()
        self.tmp.cleanup()

    def mode(self, value, accounts=None):
        (self.root/'mode').write_text(value)
        if accounts is not None: (self.root/'accounts.json').write_text(json.dumps(accounts))

    def calls(self):
        path = self.root/'calls'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def git(self, *args):
        result = subprocess.run(['git', *args], capture_output=True, text=True, cwd=self.home, env=os.environ.copy())
        return result.stdout.strip() if result.returncode == 0 else None

    def sign_in(self):
        state = {'id': 'op'}; writes = []
        try:
            auth.authorize(state, self.root/'job.json', lambda p, s: writes.append(json.loads(json.dumps(s))))
            return writes, None
        except RuntimeError as exc:
            return writes, str(exc)

    def test_account_state_is_local_and_never_reads_a_token(self):
        self.assertEqual(auth.account_info(), {'state': 'signed_out'})
        self.mode('success', ['octo-fixture'])
        self.assertEqual(auth.account_info(), {'state': 'signed_in', 'username': 'octo-fixture'})
        self.mode('garbled_config')
        self.assertEqual(auth.account_info(), {'state': 'unknown'})
        self.paths['gh'] = None
        self.assertEqual(auth.account_info(), {'state': 'unavailable'})
        self.assertTrue(all(c['args'][:2] == ['config', 'get'] and not c['token_env'] for c in self.calls()))

    def test_device_flow_configures_git_first_and_publishes_only_the_exact_challenge(self):
        writes, error = self.sign_in()
        self.assertIsNone(error)
        self.assertEqual([w['stage'] for w in writes], ['awaiting_approval', 'verifying'])
        action = writes[0]['action']
        self.assertEqual((action['url'], action['code'], action['setup_action']),
                         ('https://github.com/login/device', 'AB12-CD34', 'sign_in_github'))
        commands = [c['args'][:2] for c in self.calls()]
        self.assertLess(commands.index(['auth', 'setup-git']), commands.index(['auth', 'login']))
        login = next(c['args'] for c in self.calls() if c['args'][:2] == ['auth', 'login'])
        for flag in ('--web', '--insecure-storage', '--skip-ssh-key'):
            self.assertIn(flag, login)
        self.assertEqual(login[login.index('--git-protocol') + 1], 'https')
        self.assertNotIn('--scopes', login)
        self.assertFalse(any(c['token_env'] for c in self.calls()))

    def test_partial_url_line_is_not_misjudged(self):
        self.mode('split_url')
        writes, error = self.sign_in()
        self.assertIsNone(error); self.assertEqual(writes[0]['action']['url'], 'https://github.com/login/device')

    def test_existing_account_is_not_replaced_or_prompted(self):
        self.mode('success', ['someone-else'])
        self.assertEqual(self.sign_in(), ([], None))
        self.assertFalse(any(c['args'][:2] == ['auth', 'login'] for c in self.calls()))
        self.assertTrue(any(c['args'][:2] == ['auth', 'setup-git'] for c in self.calls()))
        self.assertEqual(self.git('config', '--global', 'user.email'), '7+someone-else@users.noreply.github.com')

    def test_config_errors_never_report_signed_out_or_successful_removal(self):
        self.mode('config_error', ['octo-fixture'])
        self.assertEqual(auth.account_info(), {'state': 'unknown'})
        with self.assertRaisesRegex(RuntimeError, 'Could not inspect'):
            auth.sign_out({}, self.root/'job.json', lambda *_: None)
        self.assertIn('Could not inspect', self.sign_in()[1])
        self.assertEqual(json.loads((self.root/'accounts.json').read_text()), ['octo-fixture'])
        self.assertFalse(any(c['args'][:2] in (['auth', 'login'], ['auth', 'logout']) for c in self.calls()))

    def test_existing_credential_helpers_are_never_overwritten(self):
        for hostname in ('github.com', 'gist.github.com'):
            key = 'credential.https://'+hostname+'.helper'
            self.git('config', '--global', key, 'store')
            writes, error = self.sign_in()
            self.assertEqual(writes, [])
            self.assertIn('another credential helper', error)
            self.assertEqual(self.git('config', '--global', key), 'store')
            self.git('config', '--global', '--unset-all', key)
        self.assertFalse(any(c['args'][:2] in (['auth', 'setup-git'], ['auth', 'login']) for c in self.calls()))

    def test_existing_gh_credential_helper_remains_compatible(self):
        self.git('config', '--global', 'credential.https://github.com.helper', '!'+str(self.binary)+' auth git-credential')
        self.assertIsNone(self.sign_in()[1])

    def test_hidden_multi_valued_helpers_are_not_discarded(self):
        helper = '!'+str(self.binary)+' auth git-credential'
        for key in ('credential.helper', 'credential.https://github.com.helper'):
            self.git('config', '--global', '--add', key, 'store')
            self.git('config', '--global', '--add', key, helper)
            self.assertIn('another credential helper', self.sign_in()[1])
            self.assertEqual(self.git('config', '--global', '--get-all', key), 'store\n'+helper)
            self.git('config', '--global', '--unset-all', key)
        self.assertFalse(any(call['args'][:2] == ['auth', 'setup-git'] for call in self.calls()))

    def test_existing_url_reset_preserves_other_hosts_global_helper(self):
        self.git('config', '--global', 'credential.helper', 'store')
        for hostname in ('github.com', 'gist.github.com'):
            key = 'credential.https://'+hostname+'.helper'
            self.git('config', '--global', '--add', key, '')
            self.git('config', '--global', '--add', key, '!'+str(self.binary)+' auth git-credential')
        self.assertIsNone(self.sign_in()[1])
        self.assertEqual(self.git('config', '--global', 'credential.helper'), 'store')

    def test_unexpected_page_or_format_is_rejected_without_a_challenge(self):
        for mode, message in (('wrong_url', 'Unexpected GitHub authorization page'), ('no_code', 'code was not recognized')):
            self.mode(mode)
            writes, error = self.sign_in()
            self.assertEqual(writes, []); self.assertIn(message, error)

    def test_git_configuration_failure_stops_before_any_login(self):
        self.mode('setup_git_fails')
        writes, error = self.sign_in()
        self.assertEqual(writes, []); self.assertIn('configure Git', error)
        self.assertFalse(any(c['args'][:2] == ['auth', 'login'] for c in self.calls()))

    def test_denial_or_unsaved_credentials_discard_upstream_output(self):
        for mode in ('denied', 'unsaved'):
            self.mode(mode, [])
            writes, error = self.sign_in()
            self.assertIsNotNone(error)
            self.assertNotIn('PRIVATE', error); self.assertNotIn('secret.invalid', error)
            self.assertNotIn('PRIVATE', json.dumps(writes))

    def test_attempt_is_bounded_and_the_cli_is_stopped(self):
        self.mode('hang')
        with patch.object(auth, 'LOGIN_SECONDS', 1):
            writes, error = self.sign_in()
        self.assertEqual(len(writes), 1); self.assertIn('expired', error)

    def test_unset_git_author_uses_the_private_noreply_address(self):
        _, error = self.sign_in()
        self.assertIsNone(error)
        self.assertEqual(self.git('config', '--global', 'user.name'), 'Octo Fixture')
        self.assertEqual(self.git('config', '--global', 'user.email'), '4242+octo-fixture@users.noreply.github.com')

    def test_owner_git_author_is_never_overwritten(self):
        self.git('config', '--global', 'user.name', 'Owner Chosen')
        _, error = self.sign_in()
        self.assertIsNone(error)
        self.assertEqual(self.git('config', '--global', 'user.name'), 'Owner Chosen')
        self.assertEqual(self.git('config', '--global', 'user.email'), '4242+octo-fixture@users.noreply.github.com')

    def test_homecloud_author_follows_a_new_account_until_the_owner_changes_it(self):
        self.assertEqual(self.sign_in()[1], None)
        self.git('config', '--global', 'user.name', 'Edited By Owner')
        auth.sign_out({}, self.root/'job.json', lambda *_: None)
        (self.root/'login_as').write_text('new-fixture')
        _, error = self.sign_in()
        self.assertIsNone(error)
        self.assertEqual(self.git('config', '--global', 'user.name'), 'Edited By Owner')
        self.assertEqual(self.git('config', '--global', 'user.email'), '99+new-fixture@users.noreply.github.com')

    def test_identity_lookup_failure_does_not_fail_sign_in(self):
        self.mode('api_fails')
        _, error = self.sign_in()
        self.assertIsNone(error)
        self.assertIsNone(self.git('config', '--global', 'user.email'))
        self.mode('no_name', [])
        _, error = self.sign_in()
        self.assertEqual(self.git('config', '--global', 'user.name'), 'octo-fixture')

    def test_partial_identity_write_preserves_origin_for_a_later_retry(self):
        self.assertIsNone(self.sign_in()[1])
        self.mode('no_name', ['new-fixture'])
        original_run = auth.run
        def fail_name(args, **kwargs):
            if args[1:4] == ['config', '--global', 'user.name']: return 1, b''
            return original_run(args, **kwargs)
        with patch.object(auth, 'run', side_effect=fail_name): auth.configure_identity('new-fixture')
        saved = json.loads(auth.identity_path().read_text())
        self.assertEqual(saved['user.name'], 'Octo Fixture')
        self.assertEqual(saved['user.email'], '99+new-fixture@users.noreply.github.com')
        auth.configure_identity('new-fixture')
        self.assertEqual(self.git('config', '--global', 'user.name'), 'new-fixture')

    def test_sign_out_removes_every_saved_account_and_is_idempotent(self):
        self.mode('success', ['octo-fixture', 'second-fixture'])
        auth.sign_out({}, self.root/'job.json', lambda *_: None)
        self.assertEqual(auth.account_info(), {'state': 'signed_out'})
        auth.sign_out({}, self.root/'job.json', lambda *_: None)
        self.mode('logout_fails', ['octo-fixture'])
        with self.assertRaisesRegex(RuntimeError, 'Could not remove GitHub access'):
            auth.sign_out({}, self.root/'job.json', lambda *_: None)


if __name__ == '__main__':
    unittest.main()
