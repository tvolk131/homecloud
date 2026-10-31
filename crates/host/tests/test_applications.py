"""Guest adapter recovery tests: no virtualization or authorization service needed."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
sys.path.insert(0, str(Path(__file__).parents[3] / 'guest'))
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('managed_guest', Path(__file__).parents[3] / 'guest/managed-app.py')
guest = importlib.util.module_from_spec(spec); spec.loader.exec_module(guest)
APP = '00000000-0000-4000-8000-000000000001'
OP = '00000000-0000-4000-8000-000000000002'

class ApplicationTests(unittest.TestCase):
    def test_challenge_parser_is_specific_and_has_a_bounded_lifetime(self):
        text = '\x1b[32mHeadless authorization\nOpen this URL on a device with a browser:\n https://example.com/connect?code=ABC\nConfirm this code when asked: ABC-123\nWaiting for approval (expires in 5 min).'
        result = guest.parse_challenge(text, 1000)
        self.assertEqual(result['code'], 'ABC-123'); self.assertEqual(result['expires_at'], 1270)
        self.assertIsNone(guest.parse_challenge('https://example.com unrelated output', 1000))

    def inspect_after_restart(self, authenticated=False, linked=False, revoked=False, boot='old', active=True):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp); boot_file = base/'boot'; boot_file.write_text('new')
            binary = base/'t3'; binary.touch()
            state = {'id': OP, 'kind': 'setup', 'unit': 'fixture.service', 'phase': 'waiting_for_user', 'boot_id': boot,
                     'action': {'url': 'https://example.com', 'code': 'OLD-CODE', 'expires_at': 99999999999}}
            guest.save(base/APP/'job.json', state)
            status = {'authenticated': authenticated, 'desired': linked, 'linked': linked, 'revoked': revoked}
            with patch.object(guest, 'BASE', base), patch.object(guest, 'BOOT', boot_file), patch.object(guest, 'T3', binary), patch.object(guest, 'command', return_value='t3 v0.1.2'), patch.object(guest, 't3_status', return_value=status), patch.object(guest.codex_auth, 'account_info', return_value={'state':'signed_out'}), patch.object(guest.claude_auth, 'account_info', return_value={'state':'signed_out'}), patch.object(guest.github_auth, 'account_info', return_value={'state':'signed_out'}), patch.object(guest.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0 if active else 3)):
                return guest.inspect(APP)

    def test_gui_or_controller_restart_preserves_live_challenge(self):
        report = self.inspect_after_restart(boot='new')
        self.assertEqual(report['observation']['installed_version'], '0.1.2')
        self.assertEqual(report['job']['phase'], 'waiting_for_user')
        self.assertEqual(report['observation']['action']['code'], 'OLD-CODE')

    def test_vm_reboot_or_dead_job_clears_incomplete_pairing(self):
        for kwargs in ({}, {'boot': 'new', 'active': False}):
            report = self.inspect_after_restart(**kwargs)
            self.assertEqual(report['job']['phase'], 'interrupted')
            self.assertIsNone(report['observation']['action'])
            self.assertEqual(report['observation']['setup'], 'interrupted')

    def test_saved_credentials_allow_recovery_without_a_new_challenge(self):
        report = self.inspect_after_restart(authenticated=True)
        self.assertEqual(report['observation']['setup'], 'finishing')
        self.assertIsNone(report['observation']['action'])

    def test_success_before_crash_is_observed_without_repeating_pairing(self):
        report = self.inspect_after_restart(authenticated=True, linked=True)
        self.assertEqual(report['job']['phase'], 'succeeded')
        self.assertEqual(report['observation']['setup'], 'ready')

    def test_revoked_credential_is_not_automatically_relinked(self):
        report = self.inspect_after_restart(authenticated=True, linked=True, revoked=True)
        self.assertNotEqual(report['observation']['setup'], 'finishing')
        self.assertNotEqual(report['observation']['setup'], 'ready')

    def test_pairing_success_does_not_hide_missing_provider_binaries(self):
        with patch.object(guest.shutil, 'which', return_value=None):
            report = self.inspect_after_restart(authenticated=True, linked=True)
        self.assertEqual(report['observation']['setup'], 'ready')
        self.assertEqual([c['name'] for c in report['observation']['components']], ['Codex CLI', 'Claude Code', 'GitHub CLI'])
        self.assertTrue(all(not c['ready'] and c['version'] is None for c in report['observation']['components']))

class CodexRecoveryTests(unittest.TestCase):
    def inspect_codex(self, phase, account, active=True, boot='new', target='sign_in_codex'):
        with tempfile.TemporaryDirectory() as tmp:
            base=Path(tmp); boot_file=base/'boot'; boot_file.write_text('new')
            binary=base/'t3'; binary.touch()
            state={'id':OP,'kind':'setup','setup_action':target,'unit':'fixture.service',
                   'phase':phase,'boot_id':boot,'action':{'url':'https://auth.openai.com/codex/device','code':'DEMO-1234','expires_at':99999999999}}
            guest.save(base/APP/'job.json',state)
            name={'sign_in_codex':'Codex CLI','sign_in_claude':'Claude Code'}.get(target,'GitHub CLI')
            components=[{'name':name,'version':'0.165.0','ready':True,'account':account}]
            with patch.object(guest,'BASE',base), patch.object(guest,'BOOT',boot_file), patch.object(guest,'T3',binary), patch.object(guest,'command',return_value='t3 v0.1.2'), patch.object(guest,'t3_status',return_value={'authenticated':True,'desired':True,'linked':True}), patch.object(guest,'provider_components',return_value=components), patch.object(guest.subprocess,'run',return_value=subprocess.CompletedProcess([],0 if active else 3)):
                return guest.inspect(APP)

    def test_t3_connection_does_not_complete_a_pending_codex_login(self):
        report=self.inspect_codex('waiting_for_user','signed_out')
        self.assertEqual(report['observation']['setup'],'ready')
        self.assertEqual(report['job']['phase'],'waiting_for_user')
        self.assertEqual(report['observation']['action']['setup_action'],'sign_in_codex')

    def test_reboot_clears_codex_challenge_without_unpairing_t3(self):
        report=self.inspect_codex('waiting_for_user','signed_out',boot='old')
        self.assertEqual(report['observation']['setup'],'ready')
        self.assertEqual(report['job']['phase'],'interrupted')
        self.assertIsNone(report['observation']['action'])
        self.assertIn('interrupted', report['observation']['operation_message'])

    def test_committed_codex_credentials_repair_interrupted_job(self):
        report=self.inspect_codex('waiting_for_user','signed_in',active=False)
        self.assertEqual(report['job']['phase'],'succeeded')
        self.assertIsNone(report['observation']['action'])

    def test_claude_recovery_is_independent_of_t3_pairing(self):
        for kwargs in ({'account':'signed_out','boot':'old'}, {'account':'signed_in','active':False}):
            report=self.inspect_codex('waiting_for_user',target='sign_in_claude',**kwargs)
            self.assertEqual(report['observation']['setup'],'ready')
            self.assertEqual(report['job']['phase'],'succeeded' if kwargs['account']=='signed_in' else 'interrupted')
            self.assertIsNone(report['observation']['action'])

    def test_github_sign_in_and_removal_recover_only_from_their_own_outcome(self):
        for target, account, phase in (('sign_in_github', 'signed_in', 'succeeded'), ('sign_in_github', 'signed_out', 'interrupted'),
                                       ('sign_out_github', 'signed_out', 'succeeded'), ('sign_out_github', 'signed_in', 'interrupted')):
            report=self.inspect_codex('running',account,active=False,target=target)
            self.assertEqual(report['observation']['setup'],'ready')
            self.assertEqual(report['job']['phase'],phase,(target,account))
            message=report['observation']['operation_message']
            if phase=='interrupted':
                self.assertIn('Removing GitHub access' if target=='sign_out_github' else 'sign-in was interrupted',message)
            elif target=='sign_out_github':
                self.assertIn('revoked in GitHub settings',message)
            else:
                self.assertIn('Git and gh now use this account',message)

    def test_unknown_setup_action_is_rejected_by_the_job_entrypoint(self):
        with patch.object(guest.sys,'argv',['managed-app.py','job',APP,OP,'setup','0.1.2','unit','sign_in_anything']):
            with self.assertRaises(AssertionError): guest.main()


class AccountCacheTests(unittest.TestCase):
    def test_completed_login_cannot_reuse_an_earlier_signed_out_observation(self):
        import time
        with tempfile.TemporaryDirectory() as tmp:
            directory=Path(tmp);now=time.time();paths={}
            for name in ['codex','claude']:
                paths[name]=str(directory/name);Path(paths[name]).touch()
                guest.save(directory/(name+'-account.json'),{'account':'signed_out','metadata':{'state':'signed_out'},'observed_at':now-5})
            def version(args,**kwargs):return '0.165.0' if Path(args[0]).name=='codex' else '2.1.308'
            with patch.object(guest.shutil,'which',side_effect=lambda name:paths.get(name)), patch.object(guest,'command',side_effect=version), patch.object(guest.codex_auth,'account_info',return_value={'state':'signed_in'}) as codex, patch.object(guest.claude_auth,'account_info',return_value={'state':'signed_in'}) as claude:
                components=guest.provider_components(directory,{'claude':now-1})
                self.assertEqual(components[1]['account'],'signed_in');claude.assert_called_once();codex.assert_not_called()
