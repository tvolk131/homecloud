"""Test supervised Claude OAuth plumbing without an account or model request."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
sys.path.insert(0,str(Path(__file__).parents[3]/'guest'))
import claude_auth as auth

OP='00000000-0000-4000-8000-000000000002'
URL='https://claude.com/cai/oauth/authorize?code=true&response_type=code&state=DEMO-STATE&code_challenge=DEMO-CHALLENGE'
FAKE='''#!/usr/bin/env python3
import json,sys
from pathlib import Path
marker=Path(MARKER)
if sys.argv[1:]==['auth','status']:
 print(json.dumps({'loggedIn':marker.exists(),'email':'PRIVATE-IDENTITY'}));sys.exit(0 if marker.exists() else 1)
print(URL,flush=True)
print('Paste code here if prompted >',flush=True)
code=sys.stdin.readline().strip()
if code!='FIXTURE-RETURN-CODE':sys.exit(1)
marker.write_text('saved')
'''

class ClaudeAuthTests(unittest.TestCase):
    def test_url_parser_is_specific_and_does_not_accept_other_destinations(self):
        result=auth.challenge('\x1b[32m'+URL,1000)
        self.assertEqual(result['expires_at'],1600);self.assertTrue(result['input_required'])
        self.assertEqual(result['setup_action'],'sign_in_claude')
        self.assertIsNotNone(auth.challenge(URL+'&response_type=code',1000))
        self.assertIsNone(auth.challenge(URL+'&response_type=token',1000))
        for url in [URL.replace('claude.com','evil.invalid'),URL.replace('response_type=code','response_type=token'),URL.replace('https:','http:'),URL.replace('/cai/oauth/authorize','/wrong')]:
            self.assertIsNone(auth.challenge(url,1000))

    def test_terminal_hyperlink_fixtures_match_the_desktop_browser_contract(self):
        fixtures=json.loads((Path(__file__).parents[3]/'guest/fixtures/claude-auth-terminal.json').read_text())
        for fixture in fixtures:
            with self.subTest(name=fixture['name']):
                result=auth.challenge(fixture['terminal'],1000)
                self.assertEqual(result['url'] if result else None,fixture['url'])
                if result:
                    self.assertTrue(result['url'].startswith('https://'))
                    self.assertFalse(any(ord(c)<32 or 127<=ord(c)<=159 for c in result['url']))
                    self.assertEqual(result['url'].count('https://'),1)

    def test_input_rejects_controls_newlines_and_oversized_values(self):
        for value in ['bad','CODE\nANOTHER', 'x'*2049, None, '☺'*10]:
            with self.assertRaises(RuntimeError):auth.input_code(value)
        self.assertEqual(auth.input_code('code_part#state-part'),b'code_part#state-part')

    def run_flow(self, invalid=False, already=False, expire=False, bad_url=False, hyperlink=False):
        with tempfile.TemporaryDirectory() as tmp:
            directory=Path(tmp);binary=directory/'claude';marker=directory/'authenticated'
            url = URL.replace('/cai/oauth/authorize','/unsupported') if bad_url else URL
            terminal_url = '\x1b]8;;'+url+'\x07'+url+'\x1b]8;;\x07' if hyperlink else url
            binary.write_text(FAKE.replace('MARKER',repr(str(marker))).replace('URL',repr(terminal_url)));binary.chmod(0o700)
            if already:marker.write_text('existing')
            boot=directory/'boot';boot.write_text('fixture-boot')
            state={'id':OP,'kind':'setup','setup_action':'sign_in_claude','boot_id':'fixture-boot','phase':'running'}
            writes=[];failure=[];path=directory/'job.json'
            def save(p,s):
                # Test writers are sequential except for reading; mimic atomic replace.
                value=json.loads(json.dumps(s));writes.append(value)
                temp=p.with_suffix('.new');temp.write_text(json.dumps(value));temp.replace(p)
            def worker():
                try:auth.authorize(state,path,save)
                except Exception as error:failure.append(str(error))
            with patch.object(auth.shutil,'which',return_value=str(binary)), patch.object(auth,'BOOT',boot), patch.object(auth,'LOGIN_SECONDS',1 if expire else 600):
                task=threading.Thread(target=worker);task.start()
                deadline=time.monotonic()+10
                while time.monotonic()<deadline:
                    if (path.exists() and json.loads(path.read_text()).get('phase')=='waiting_for_user') or not task.is_alive():break
                    time.sleep(.02)
                if not already and not expire and not bad_url:
                    self.assertTrue(path.exists(), 'Login fixture did not publish readiness: '+str(failure))
                    self.assertEqual(json.loads(path.read_text()).get('phase'),'waiting_for_user')
                    auth.submit(directory,OP,'WRONG-RETURN-CODE' if invalid else 'FIXTURE-RETURN-CODE',lambda p:json.loads(p.read_text()))
                task.join(timeout=5);self.assertFalse(task.is_alive())
                self.assertFalse((directory/(OP+'.input')).exists())
                self.assertNotIn('FIXTURE-RETURN-CODE',json.dumps(writes));self.assertNotIn('PRIVATE-IDENTITY',json.dumps(writes))
                return writes,failure,marker.read_text() if marker.exists() else None

    def test_browser_code_reaches_same_process_without_becoming_persistent_state(self):
        writes,failure,saved=self.run_flow()
        self.assertFalse(failure);self.assertEqual(saved,'saved')
        self.assertEqual([s['stage'] for s in writes],['awaiting_approval','verifying'])
        self.assertNotIn('action',writes[-1])

    def test_hyperlinked_cli_url_and_return_code_complete_the_same_process(self):
        writes,failure,saved=self.run_flow(hyperlink=True)
        self.assertFalse(failure);self.assertEqual(saved,'saved')
        self.assertEqual(writes[0]['action']['url'],URL)

    def test_existing_account_is_preserved_without_starting_login(self):
        writes,failure,saved=self.run_flow(already=True)
        self.assertEqual((writes,failure,saved),([],[],'existing'))

    def test_bad_code_and_expiry_finish_with_retryable_error(self):
        for mode in [{'invalid':True},{'expire':True}]:
            writes,failure,saved=self.run_flow(**mode)
            self.assertTrue(failure);self.assertIsNone(saved)
            self.assertNotIn('WRONG-RETURN-CODE',str(failure))

    def test_submission_rejects_wrong_or_finished_job_and_regular_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory=Path(tmp);boot=directory/'boot';boot.write_text('boot')
            state={'id':OP,'kind':'setup','setup_action':'sign_in_claude','phase':'waiting_for_user','boot_id':'boot',
                'action':{'input_required':True,'expires_at':time.time()+30}}
            read=lambda p:state
            with patch.object(auth,'BOOT',boot):
                with self.assertRaises(RuntimeError):auth.submit(directory,'wrong-job','FIXTURE-CODE',read)
                state['phase']='succeeded'
                with self.assertRaises(RuntimeError):auth.submit(directory,OP,'FIXTURE-CODE',read)
                state['phase']='waiting_for_user';(directory/(OP+'.input')).write_text('existing')
                with self.assertRaises(RuntimeError):auth.submit(directory,OP,'FIXTURE-CODE',read)
                self.assertEqual((directory/(OP+'.input')).read_text(),'existing')

if __name__=='__main__':unittest.main()


class UnsupportedLoginTests(unittest.TestCase):
    run_flow = ClaudeAuthTests.run_flow
    def test_unsupported_url_fails_explicitly_instead_of_waiting_silently(self):
        _,failure,saved=self.run_flow(bad_url=True)
        self.assertTrue(failure);self.assertIsNone(saved)
        self.assertIn('format was not recognized',failure[0])
