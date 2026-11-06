"""Offline safety checks for the nested Proxmox supervisor."""

import argparse
import ast
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

import proxmox_spike


class ProxmoxSpikeTests(unittest.TestCase):
    def guest_functions(self):
        tree = ast.parse(Path(__file__).with_name('proxmox_guest.py').read_text())
        tree.body = [entry for entry in tree.body if isinstance(entry, (ast.Import, ast.ImportFrom, ast.FunctionDef))]
        namespace = {}
        exec(compile(tree, '<proxmox-guest-functions>', 'exec'), namespace)
        return namespace

    def test_remote_program_compiles(self):
        compile(proxmox_spike.HOST_PROGRAM, '<proxmox-host>', 'exec')
        ast.parse(Path(__file__).with_name('proxmox_guest.py').read_text())

    def test_invalid_identity_is_rejected_before_host_mutation(self):
        state = {'run_id': '../wrong', 'instance': 'wrong', 'storage': 'wrong',
                 'network': 'wrong', 'dataset': 'hclab'}
        request = {'state': state, 'action': 'cleanup'}
        with patch('sys.stdin', io.StringIO(json.dumps(request))):
            with patch('subprocess.run') as command:
                with self.assertRaisesRegex(RuntimeError, 'Invalid run identity'):
                    exec(proxmox_spike.HOST_PROGRAM, {})
                command.assert_not_called()

    def test_mismatched_dataset_is_rejected_before_host_mutation(self):
        token = 'a' * 32
        state = {'run_id': token, 'instance': 'hc-pve-' + token[:10],
                 'storage': 'hcpve-' + token[:10], 'network': 'hcvmsp' + token[:8],
                 'dataset': 'hclab/homecloud'}
        with patch('sys.stdin', io.StringIO(json.dumps({'state': state, 'action': 'cleanup'}))):
            with patch('subprocess.run') as command:
                with self.assertRaisesRegex(RuntimeError, 'Resource identity mismatch'):
                    exec(proxmox_spike.HOST_PROGRAM, {})
                command.assert_not_called()

    def test_original_snapshot_excludes_keys_and_credentials(self):
        response = {'kind': 'snapshot', 'data': {'vms': [{
            'id': 'vm-id', 'spec': {'name': 'devbox', 'ssh_public_key': 'not-for-manifest'},
            'desired': 'running', 'observed': 'running', 'ssh_host_key': 'not-for-manifest',
        }]}}
        completed = subprocess.CompletedProcess([], 0, json.dumps(response), '')
        with patch('subprocess.run', return_value=completed):
            result = proxmox_spike.snapshot(argparse.Namespace(cli='/test/homecloud'))
        self.assertEqual(result['original_vms'], [{'id': 'vm-id', 'name': 'devbox',
                                                 'desired': 'running', 'observed': 'running'}])
        self.assertNotIn('not-for-manifest', json.dumps(result))

    def test_cleanup_refuses_a_vm_with_a_foreign_tag(self):
        token = 'b' * 32
        state = {'run_id': token, 'instance': 'hc-pve-' + token[:10],
                 'storage': 'hcpve-' + token[:10], 'network': 'hcvmsp' + token[:8],
                 'dataset': 'hclab/proxmox-spike-' + token}
        response = subprocess.CompletedProcess([], 0, json.dumps({'config': {'user.homecloud.spike': 'different'}}), '')
        for force in (False, True):
            with self.subTest(force_worker_stop=force):
                request = {'state': state, 'action': 'cleanup', 'force_worker_stop': force}
                with patch('sys.stdin', io.StringIO(json.dumps(request))):
                    with patch('subprocess.run', return_value=response) as command:
                        with self.assertRaisesRegex(RuntimeError, 'Foreign VM'):
                            exec(proxmox_spike.HOST_PROGRAM, {})
                        self.assertEqual(command.call_count, 1)
                        self.assertIn('query', command.call_args[0][0])

    def test_remote_failure_does_not_accept_partial_json(self):
        completed = subprocess.CompletedProcess([], 1, '{"success":true}', 'failed')
        args = argparse.Namespace(ssh_config='/test/ssh_config')
        with patch('subprocess.run', return_value=completed):
            with self.assertRaisesRegex(RuntimeError, 'failed'):
                proxmox_spike.remote(args, {'action': 'probe'})

    def test_cleanup_preserves_an_externally_changed_cpu_policy(self):
        token = 'c' * 32
        state = {'run_id': token, 'instance': 'hc-pve-' + token[:10],
                 'storage': 'hcpve-' + token[:10], 'network': 'hcvmsp' + token[:8],
                 'dataset': 'hclab/proxmox-spike-' + token, 'controller_was_active': True}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            run_directory = root / 'test-runs' / ('proxmox-' + token)
            run_directory.mkdir(parents=True)
            (run_directory / 'cpu-policy.original').write_bytes(b'original\n')
            policy = root / 'cpu-policy.tsv'
            changed = b'external-change\n' + (state['instance'] + '\t0\t0\t/lab\n').encode()
            policy.write_bytes(changed)
            paths = {'/var/lib/homecloud/test-runs': root / 'test-runs',
                     '/etc/homecloud/cpu-policy.tsv': policy}
            missing = subprocess.CompletedProcess([], 1, '', 'not found')
            request = {'state': state, 'action': 'cleanup'}
            with patch('sys.stdin', io.StringIO(json.dumps(request))):
                with patch('pathlib.Path', side_effect=lambda path: paths[path]):
                    with patch('subprocess.run', return_value=missing) as command:
                        with self.assertRaisesRegex(RuntimeError, 'externally changed CPU policy'):
                            exec(proxmox_spike.HOST_PROGRAM, {})
            self.assertEqual(policy.read_bytes(), changed)
            self.assertFalse(any(call.args[0][0] == 'systemctl' for call in command.call_args_list))

    def test_cleaned_manifest_cannot_start_another_operation(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'state.json'
            path.write_text(json.dumps({'cleaned': True}))
            with patch('sys.argv', ['proxmox_spike.py', 'prepare', '--state', str(path)]):
                with patch.object(proxmox_spike, 'remote') as command:
                    with self.assertRaisesRegex(RuntimeError, 'use a new manifest'):
                        proxmox_spike.main()
                    command.assert_not_called()

    def test_force_worker_stop_is_only_valid_for_cleanup(self):
        arguments = ['proxmox_spike.py', 'prepare', '--state', '/unused', '--force-worker-stop']
        with patch('sys.argv', arguments), patch('sys.stderr', io.StringIO()):
            with self.assertRaises(SystemExit) as failure:
                proxmox_spike.main()
        self.assertEqual(failure.exception.code, 2)

    def test_cleanup_keeps_the_failed_phase_history(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'state.json'
            path.write_text(json.dumps({'observations': {}, 'original_vms': []}))
            args = argparse.Namespace(action='probe', force_worker_stop=False)
            with patch.object(proxmox_spike, 'remote', side_effect=RuntimeError('test phase failed')):
                with self.assertRaisesRegex(RuntimeError, 'test phase failed'):
                    proxmox_spike.execute(args, path)
            failed = json.loads(path.read_text())
            self.assertEqual(failed['failures'], [failed['failure']])
            args.action = 'cleanup'
            with patch.object(proxmox_spike, 'remote', return_value={'cleaned': True}):
                with patch.object(proxmox_spike, 'snapshot', return_value={'original_vms': []}):
                    with patch('sys.stdout', io.StringIO()):
                        proxmox_spike.execute(args, path)
            cleaned = json.loads(path.read_text())
            self.assertTrue(cleaned['cleaned'])
            self.assertNotIn('failure', cleaned)
            self.assertEqual(cleaned['failures'], failed['failures'])

    def test_t3_status_waits_for_http_and_service(self):
        namespace = self.guest_functions()
        namespace['guest_command'] = Mock(side_effect=[
            'activating\nt3 v0.1.2\n000\nboot-id\n1',
            'active\nt3 v0.1.2\n200\nboot-id\n2',
        ])
        with patch.object(namespace['time'], 'sleep') as sleep:
            result = namespace['t3_status'](9001)
        self.assertEqual(result, ['active', 't3 v0.1.2', '200', 'boot-id', '2'])
        sleep.assert_called_once_with(3)
        self.assertEqual(namespace['guest_command'].call_count, 2)

    def test_readiness_requires_all_proxmox_services_active(self):
        namespace = self.guest_functions()
        namespace['run'] = Mock(side_effect=[
            subprocess.CompletedProcess([], 0, 'inactive\nactivating\nactive\n', ''),
            subprocess.CompletedProcess([], 0, 'active\nactive\nactive\n', ''),
        ])
        with patch.object(namespace['time'], 'sleep') as sleep:
            self.assertEqual(namespace['wait_services'](), 'active\nactive\nactive')
        sleep.assert_called_once_with(3)
        self.assertEqual(namespace['run'].call_count, 2)

    def test_api_get_and_delete_parameters_use_query_not_body(self):
        for method in ('GET', 'DELETE'):
            with self.subTest(method=method):
                namespace = self.guest_functions()
                namespace['DIRECTORY'] = Path('/unused')
                credential = {'full-tokenid': 'test@pve!lab', 'value': 'test-only'}
                with patch.object(namespace['urllib'].request, 'urlopen', return_value=io.StringIO('{"data":"ok"}')) as request:
                    self.assertEqual(namespace['api'](method, '/nodes/lab/qemu/9001', {'purge': 1}, credential), 'ok')
                sent = request.call_args.args[0]
                self.assertEqual(sent.get_method(), method)
                self.assertTrue(sent.full_url.endswith('/nodes/lab/qemu/9001?purge=1'))
                self.assertIsNone(sent.data)

    def test_api_post_keeps_json_parameters(self):
        namespace = self.guest_functions()
        namespace['DIRECTORY'] = Path('/unused')
        credential = {'full-tokenid': 'test@pve!lab', 'value': 'test-only'}
        with patch.object(namespace['urllib'].request, 'urlopen', return_value=io.StringIO('{"data":"ok"}')) as request:
            self.assertEqual(namespace['api']('POST', '/nodes/lab/qemu', {'vmid': 9001}, credential), 'ok')
        sent = request.call_args.args[0]
        self.assertEqual(sent.get_method(), 'POST')
        self.assertEqual(json.loads(sent.data), {'vmid': 9001})

    def test_t3_status_rejects_an_unexpected_version(self):
        namespace = self.guest_functions()
        namespace['guest_command'] = Mock(return_value='active\nt3 v99.0.0\n200\nboot-id\n2')
        with self.assertRaisesRegex(RuntimeError, 'Unexpected T3 version'):
            namespace['t3_status'](9001)


if __name__ == '__main__':
    unittest.main()
