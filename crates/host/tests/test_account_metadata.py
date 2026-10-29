"""Account display contracts and cache boundaries. No credentials or network required."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).parents[3]
sys.path.insert(0, str(ROOT/'guest'))
import account_metadata as metadata

spec = importlib.util.spec_from_file_location('metadata_guest', ROOT/'guest/managed-app.py')
guest = importlib.util.module_from_spec(spec); spec.loader.exec_module(guest)


class MetadataTests(unittest.TestCase):
    def test_account_display_is_allowlisted_and_never_exports_tokens(self):
        raw = {'account': {'type':'chatgpt','email':'dev@example.com','planType':'pro', 'accessToken':'SECRET'},
               'workspaceRouting': {'chatgptAccountId':'workspace-fixture','bearer':'SECRET'}, 'refreshToken':'SECRET'}
        value = metadata.codex_account(raw)
        self.assertEqual(value['email'], 'dev@example.com')
        self.assertEqual(value['account_id'], 'workspace-fixture')
        self.assertEqual(value['state'], 'signed_in')
        self.assertNotIn('SECRET', json.dumps(value))
        value = metadata.claude_account({'loggedIn':True,'email':'dev@example.org','subscriptionType':'max','authMethod':'claude.ai','accessToken':'SECRET'}, 0)
        self.assertEqual(value['plan'], 'max')
        self.assertNotIn('SECRET', json.dumps(value))
        self.assertEqual(metadata.claude_account({'loggedIn':False,'email':'stale@example.org'}, 1), {'state':'signed_out'})

    def test_control_sequences_and_oversized_identities_are_not_displayed(self):
        for value in ['bad\nemail', '\x1b[31mred', 'wrong\u202eexample.com', 'x'*321, None, 42]:
            self.assertIsNone(metadata.display_text(value))
        self.assertEqual(metadata.codex_account([]), {'state':'unknown'})
        self.assertEqual(metadata.claude_account([], 0), {'state':'unknown'})

    def test_account_inspection_ignores_legacy_usage_cache_and_never_requests_limits(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp); binary = directory/'codex'; binary.touch()
            legacy = {'identity':['old@example.com','old','pro','chatgpt'], 'observed_at':time.time(),
                      'windows':[{'used_percent':99}]}
            guest.save(directory/'codex-usage.json', legacy)
            with patch.object(guest.shutil,'which',side_effect=lambda name: str(binary) if name=='codex' else None), \
                 patch.object(guest,'command',return_value='codex 0.165.0'), \
                 patch.object(guest.codex_auth,'Session') as sessions:
                session = sessions.return_value.__enter__.return_value
                def read_account(method, params):
                    self.assertEqual(method, 'account/read')
                    self.assertEqual(params, {'refreshToken':False})
                    return {'account':{'type':'chatgpt','email':'new@example.com','planType':'pro'}}
                session.request.side_effect = read_account
                result = guest.provider_components(directory)
                self.assertEqual(result[0]['metadata']['email'], 'new@example.com')
                self.assertNotIn('usage', result[0]['metadata'])
                self.assertNotIn('usage_message', result[0]['metadata'])
                session.request.assert_called_once_with('account/read', {'refreshToken':False})
                self.assertEqual(guest.read(directory/'codex-usage.json'), legacy)

    def test_old_account_cache_upgrades_without_replacing_saved_login(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp); binary = directory/'claude'; binary.touch()
            guest.save(directory/'claude-account.json', {'account':'signed_in','observed_at':time.time()})
            with patch.object(guest.shutil,'which',side_effect=lambda name: str(binary) if name=='claude' else None), \
                 patch.object(guest,'command',return_value='claude 2.1.308'), \
                 patch.object(guest.claude_auth,'account_info',return_value={'state':'signed_in','email':'dev@example.org','plan':'max'}) as inspect:
                result = guest.provider_components(directory)
                self.assertEqual(result[1]['metadata']['email'], 'dev@example.org')
                inspect.assert_called_once()
