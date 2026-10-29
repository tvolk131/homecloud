"""Exercise the real JSONL transport against a disposable fake Codex executable."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).parents[3] / 'guest'))
import codex_auth as auth

FAKE = '''#!/usr/bin/env python3
import json,sys
mode = MODE
signed = mode == 'already'
for line in sys.stdin:
 value=json.loads(line); method=value['method']; params=value.get('params',{}); id=value.get('id')
 if method=='initialized': continue
 if method=='initialize': result={}
 elif method=='account/read': result={'account': {'type':'chatgpt','email':'PRIVATE-IDENTITY'} if signed else None}
 elif method=='account/login/start':
  if mode=='error':
   print(json.dumps({'id':id,'error':{'code':-1,'message':'PRIVATE-TOKEN https://secret.invalid'}}),flush=True); continue
  result={'type':'chatgptDeviceCode','loginId':'00000000-0000-4000-8000-000000000003',
          'verificationUrl':'https://auth.openai.com/codex/device','userCode':'DEMO-1234'}
  if mode=='wrong_url': result['verificationUrl']='https://evil.invalid/capture'
 elif method=='account/login/cancel': result={}
 else: sys.exit(4)
 print(json.dumps({'id':id,'result':result}),flush=True)
 if method=='account/login/start':
  print(json.dumps({'method':'account/login/completed','params':{'loginId':'wrong-job','success':True}}),flush=True)
  signed=mode!='unsaved'
  print(json.dumps({'method':'account/login/completed','params':{'loginId':result['loginId'],'success':mode!='denied'}}),flush=True)
'''

class CodexAuthTests(unittest.TestCase):
    def flow(self, mode):
        with tempfile.TemporaryDirectory() as tmp:
            binary=Path(tmp)/'codex'; binary.write_text(FAKE.replace('MODE',repr(mode))); binary.chmod(0o700)
            state={}; writes=[]
            with patch.object(auth.shutil,'which',return_value=str(binary)):
                try:
                    auth.authorize(state, Path(tmp)/'job.json', lambda p,s: writes.append(json.loads(json.dumps(s))))
                    error=None
                except RuntimeError as exc: error=str(exc)
            return writes,error

    def test_account_metadata_uses_the_supported_read_only_jsonl_method(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary=Path(tmp)/'codex';binary.write_text(FAKE.replace('MODE',repr('already')));binary.chmod(0o700)
            with patch.object(auth.shutil,'which',return_value=str(binary)):
                info=auth.account_info()
            self.assertEqual(info['state'],'signed_in')
            self.assertEqual(info['email'],'PRIVATE-IDENTITY')

    def test_structured_login_correlates_completion_and_confirms_saved_account(self):
        writes,error=self.flow('success'); self.assertIsNone(error)
        self.assertEqual(len(writes),2); self.assertEqual(writes[0]['phase'],'waiting_for_user')
        self.assertEqual(writes[0]['action']['setup_action'],'sign_in_codex')
        self.assertNotIn('PRIVATE-IDENTITY',json.dumps(writes))

    def test_existing_account_is_not_replaced_or_prompted(self):
        self.assertEqual(self.flow('already'),([],None))

    def test_failure_discards_upstream_secrets_and_has_actionable_copy(self):
        writes,error=self.flow('error'); self.assertEqual(writes,[])
        self.assertIn('Settings > Security',error); self.assertNotIn('PRIVATE',error); self.assertNotIn('secret.invalid',error)

    def test_arbitrary_browser_url_is_rejected(self):
        writes,error=self.flow('wrong_url'); self.assertEqual(writes,[])
        self.assertIn('Unexpected Codex authorization page',error)

    def test_denial_or_notification_without_saved_credentials_is_not_success(self):
        for mode in ('denied','unsaved'):
            writes,error=self.flow(mode); self.assertEqual(len(writes),1 if mode=='denied' else 2); self.assertIsNotNone(error)

    def test_timeout_and_oversized_or_malformed_frames_are_bounded(self):
        with tempfile.TemporaryDirectory() as tmp:
            binary=Path(tmp)/'codex'
            for body in ("import time; time.sleep(5)", "print('x'*4096,flush=True)", "print('not-json',flush=True)"):
                binary.write_text('#!/usr/bin/env python3\n'+body+'\n'); binary.chmod(0o700)
                with patch.object(auth.shutil,'which',return_value=str(binary)), patch.object(auth,'MAX_FRAME',1024):
                    session=auth.Session()
                    try:
                        with self.assertRaises(RuntimeError): session.request('initialize',{},timeout=0.15)
                    finally: session.close()
                    self.assertIsNotNone(session.proc.poll())

    def test_challenge_has_a_local_waiting_deadline_not_an_invented_upstream_expiry(self):
        _,challenge=auth.challenge({'type':'chatgptDeviceCode','loginId':'00000000-0000-4000-8000-000000000003',
          'verificationUrl':'https://auth.openai.com/codex/device','userCode':'DEMO-1234'},1000)
        self.assertEqual(challenge['expires_at'],1600)

if __name__=='__main__': unittest.main()
