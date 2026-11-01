"""The host hands each VM one fixed guest runner and only invokes that binary."""
import hashlib
import json
import subprocess
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import managed_apps as m

VM = {'id': '00000000-0000-4000-8000-000000000001'}
APP = {'id': '00000000-0000-4000-8000-000000000002', 'vm_id': VM['id'], 'kind': 't3',
       'desired_version': '0.1.2', 'start_on_boot': True}
OP = '00000000-0000-4000-8000-000000000003'
REPORT = {'observation': {'setup': 'ready', 'action': None, 'operation_message': ''}, 'job': None}


class GuestRunnerTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(); self.addCleanup(temp.cleanup)
        self.binary = Path(temp.name) / 'homecloud-guest'; self.binary.write_bytes(b'\x7fELF runner')
        self.installed = None; self.calls = []
        self.active_jobs=False; self.push_fails=False; self.cleanup_fails=False
        self.jobs_query_fails=False; self.staging_cleanup_times_out=False
        self.legacy_present=True

    def application(self, action):
        def run(*args, check=True, **kwargs):
            self.calls.append(args)
            if args[:2] == ('incus', 'file'):
                if self.push_fails:raise RuntimeError('push timed out')
                self.installed = Path(args[-2]).read_bytes(); return SimpleNamespace(stdout='', returncode=0)
            command = args[4:]
            if command[:1] == ('sha256sum',):
                if self.installed is None: return SimpleNamespace(stdout='', returncode=1)
                return SimpleNamespace(stdout=hashlib.sha256(self.installed).hexdigest() + '  ' + m.RUNNER, returncode=0)
            if command[:2] == ('systemctl', 'is-active'): return SimpleNamespace(stdout='', returncode=3)
            if command[:1] == ('find',):return SimpleNamespace(stdout=m.GUEST_DIR+'/managed-app.py' if self.legacy_present else '',returncode=0)
            if command[:2] == ('systemctl', 'list-units'):
                if self.jobs_query_fails=='timeout':raise subprocess.TimeoutExpired('systemctl list-units',10)
                return SimpleNamespace(stdout='homecloud-app-fixture.service active running' if self.active_jobs else '',returncode=1 if self.jobs_query_fails else 0)
            if command[:2] == ('rm','-f') and any(part.startswith(m.RUNNER+'.new-') for part in command) and self.staging_cleanup_times_out:raise subprocess.TimeoutExpired('incus exec',10)
            if command[:2] == ('rm','-f') and m.GUEST_DIR+'/managed-app.py' in command and self.cleanup_fails:raise RuntimeError('legacy cleanup failed')
            if command[:2] == ('rm','-f') and m.GUEST_DIR+'/managed-app.py' in command:self.legacy_present=False
            if 'inspect' in command: return SimpleNamespace(stdout=json.dumps(REPORT), returncode=0)
            return SimpleNamespace(stdout='', returncode=0)
        runtime = SimpleNamespace(owned=lambda vm: None, state=lambda vm: 'running', vm_name=lambda vm: 'fixture', run=run)
        with patch.object(m, 'RUNNER_SOURCE', self.binary):
            return m.application(runtime, VM, APP, action)

    def pushes(self):
        return [c for c in self.calls if c[:3] == ('incus', 'file', 'push')]

    def test_runner_is_pushed_once_then_replaced_only_when_its_hash_changes(self):
        self.application({'kind': 'inspect'})
        self.assertEqual(len(self.pushes()), 1)
        push = self.pushes()[0]
        self.assertEqual(push[3:9], ('--uid', '0', '--gid', '0', '--mode', '0755'))
        staged = push[-1].removeprefix('fixture')
        self.assertTrue(staged.startswith(m.RUNNER + '.new-'))
        rename = next(c for c in self.calls if c[4:5] == ('mv',))
        self.assertEqual(rename[4:], ('mv', '-f', '-T', staged, m.RUNNER))
        removal = next(c for c in self.calls if m.GUEST_DIR + '/managed-app.py' in c)
        self.assertIn(m.GUEST_DIR + '/managed-app.py', removal)

        self.calls.clear(); self.application({'kind': 'inspect'})
        self.assertEqual(self.pushes(), [])
        self.assertFalse(any(call[4:6]==('systemctl','list-units') or m.GUEST_DIR+'/managed-app.py' in call for call in self.calls))
        self.binary.write_bytes(b'\x7fELF newer runner')
        self.calls.clear(); self.application({'kind': 'inspect'})
        self.assertEqual(len(self.pushes()), 1)

    def test_active_legacy_jobs_keep_helpers_until_a_later_matching_hash_inspection(self):
        self.active_jobs=True
        self.application({'kind':'inspect'})
        self.assertFalse(any(m.GUEST_DIR+'/managed-app.py' in call for call in self.calls))
        self.calls.clear();self.active_jobs=False
        self.application({'kind':'inspect'})
        self.assertEqual(self.pushes(),[])
        self.assertTrue(any(m.GUEST_DIR+'/managed-app.py' in call for call in self.calls))

    def test_failed_push_attempts_cleanup_of_its_own_staged_binary(self):
        self.push_fails=True
        for cleanup_times_out in [False,True]:
            self.calls.clear();self.staging_cleanup_times_out=cleanup_times_out
            with self.assertRaisesRegex(RuntimeError,'push timed out'):self.application({'kind':'inspect'})
            staged=self.pushes()[0][-1].removeprefix('fixture')
            self.assertTrue(any(call[4:]==('rm','-f',staged) for call in self.calls))

    def test_unknown_job_activity_preserves_legacy_helpers(self):
        for failure in [True,'timeout']:
            self.calls.clear();self.jobs_query_fails=failure
            self.application({'kind':'inspect'})
            self.assertFalse(any(m.GUEST_DIR+'/managed-app.py' in call for call in self.calls))

    def test_matching_hash_retries_failed_legacy_cleanup_without_another_push(self):
        self.cleanup_fails=True
        self.application({'kind':'inspect'})
        self.calls.clear();self.cleanup_fails=False
        self.application({'kind':'inspect'})
        self.assertEqual(self.pushes(),[])
        self.assertTrue(any(m.GUEST_DIR+'/managed-app.py' in call for call in self.calls))

    def test_owner_commands_and_jobs_run_only_the_fixed_binary(self):
        operation = {'id': OP, 'application_id': APP['id'], 'kind': 'setup', 'setup_action': 'sign_in_github'}
        self.application({'kind': 'start_setup', 'operation': operation})
        owner = [c[c.index('--', 4) + 1:] for c in self.calls if 'runuser' in c]
        self.assertTrue(owner)
        for command in owner:
            program = next(part for part in command if not part.startswith(('env', 'HOME=', 'PATH=', 'XDG_', 'DBUS_')))
            self.assertEqual(program, m.RUNNER)
        job = next(c for c in self.calls if 'systemd-run' in c)
        self.assertEqual(job[job.index(m.RUNNER):], (m.RUNNER, 'job', APP['id'], OP, 'setup', '0.1.2',
                         'homecloud-app-' + APP['id'].replace('-', '') + '-' + OP.replace('-', '') + '.service',
                         'sign_in_github'))
        self.assertFalse(any('python3' in c for c in self.calls))

    def test_unknown_setup_action_is_rejected_before_starting_a_job(self):
        operation = {'id': OP, 'application_id': APP['id'], 'kind': 'setup', 'setup_action': 'sign_in_anything'}
        with self.assertRaises(AssertionError):
            self.application({'kind': 'start_setup', 'operation': operation})
        self.assertFalse(any('systemd-run' in c for c in self.calls))


if __name__ == '__main__':
    unittest.main()
