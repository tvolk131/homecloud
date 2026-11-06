import copy
import contextlib
import io
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

import proxmox_power_live as power


def successful_validation():
    cases = {name: {'ok': True, 'result': {'legacy_vms_created': 0, 'observations': {}}}
             for name in ('start', 'shutdown', 'resume', 'force-stop', 'reader', 'invalid', 'template',
                          'seed-hold', 'hold', 'authorization')}
    tasks = []
    native = {'before': {'power_tasks': [], 'qm_status': {'7101': 'stopped', '7199': 'stopped'}, 'task_status': {}}}
    for name, worker in (('start', 'qmstart'), ('shutdown', 'qmshutdown'), ('force-stop', 'qmstop')):
        upid = 'UPID:owned:' + worker
        cases[name]['result']['observations']['running'] = {'task_id': upid}
        tasks.append({'upid': upid, 'type': worker})
    for name in cases:
        native[name] = {'power_tasks': copy.deepcopy(tasks),
                        'qm_status': {'7101': 'stopped', '7199': 'stopped'},
                        'task_status': {entry['upid']: {'status': 'stopped', 'exitstatus': 'OK' if entry['type'] != 'qmshutdown' else 'timeout'} for entry in tasks}}
    native['resume']['qm_status']['7101'] = 'running'
    native['start']['qm_status']['7101'] = 'running'
    native['shutdown']['task_status']['UPID:owned:qmshutdown'] = {'status': 'running'}
    cases['resume']['result']['observations']['terminal'] = {'phase': 'failed'}
    return {'cases': cases, 'native': native, 'probe_uid': 1000, 'token_values_redacted': True, 'failure': None}


class PowerHarnessTests(unittest.TestCase):
    def test_assessment_requires_native_task_and_state_agreement(self):
        value = successful_validation()
        self.assertTrue(power.assess(value)['passed'])
        value['native']['resume']['qm_status']['7101'] = 'stopped'
        self.assertFalse(power.assess(value)['passed'])

    def test_native_duplicate_task_is_not_success(self):
        value = successful_validation()
        value['native']['authorization']['power_tasks'].append({'upid': 'duplicate', 'type': 'qmstart'})
        self.assertIn('Native power task identities/count differ; possible duplicate dispatch', power.assess(value)['failures'])

    def test_native_failed_start_cannot_be_reported_successful(self):
        value = successful_validation()
        value['native']['start']['task_status']['UPID:owned:qmstart']['exitstatus'] = 'ERROR'
        self.assertIn('start: native task completion contradicts operation result', power.assess(value)['failures'])

    def test_negative_case_cannot_add_task_or_mutate_state(self):
        value = successful_validation()
        value['native']['reader']['power_tasks'] = []
        value['native']['reader']['qm_status']['7101'] = 'running'
        self.assertIn('reader: negative test dispatched a native task', power.assess(value)['failures'])
        self.assertIn('reader: negative test mutated power state', power.assess(value)['failures'])

    def test_incomplete_evidence_is_failed(self):
        self.assertFalse(power.assess({})['passed'])
        value = successful_validation()
        value['cases']['invalid']['ok'] = False
        self.assertFalse(power.assess(value)['passed'])

    def test_wrapper_imports_existing_host_without_running_its_main(self):
        namespace = {'__name__': 'power_test'}
        exec(compile(power.wrapped('proxmox_cockpit_host', 'proxmox_power_host.py'), '<power-host>', 'exec'), namespace)
        self.assertEqual(namespace['cockpit'].identities(self.identity())[0], 'a' * 32)
        foreign = self.identity()
        foreign['instance'] = 'not-owned'
        with self.assertRaisesRegex(RuntimeError, 'Resource identity mismatch'):
            namespace['cockpit'].identities(foreign)

    def identity(self):
        return {'run_id': 'a' * 32, 'instance': 'hc-pve-' + 'a' * 10,
                'storage': 'hcpve-' + 'a' * 10, 'network': 'hcvmsp' + 'a' * 8,
                'dataset': 'hclab/proxmox-spike-' + 'a' * 32}

    def test_cleanup_runs_after_prepare_install_build_or_validation_failure(self):
        for failure_at in ('prepare', 'install', 'build', 'validate'):
            with self.subTest(failure_at=failure_at), tempfile.TemporaryDirectory() as directory:
                filename = Path(directory) / 'manifest.json'
                args = SimpleNamespace(action='run', state=str(filename))
                calls = []

                def phase(arguments, state, action):
                    calls.append(action)
                    if action == failure_at:
                        raise RuntimeError('bounded failure')
                    return {}

                with patch.object(power.cockpit, 'helper', return_value={'controller': 'active'}), patch.object(power, 'phase', side_effect=phase):
                    with self.assertRaisesRegex(RuntimeError, 'bounded failure'):
                        power.execute(args)
                self.assertEqual(calls[-1], 'cleanup')
                self.assertTrue(json.loads(filename.read_text())['cleaned'])
                self.assertEqual(filename.stat().st_mode & 0o777, 0o600)

    def test_probe_cleanup_failure_still_destroys_fixture(self):
        with patch.object(power, 'helper', side_effect=RuntimeError('probe stop failed')), patch.object(power.cockpit, 'phase', return_value={}) as cleanup:
            with self.assertRaisesRegex(RuntimeError, 'probe stop failed'):
                power.phase(None, {}, 'cleanup')
            cleanup.assert_called_once_with(None, {}, 'cleanup')

    def test_cancelled_build_cannot_claim_success_without_executables(self):
        namespace = {'__name__': 'power_build_test'}
        exec(compile(power.wrapped('proxmox_cockpit_host', 'proxmox_power_host.py'), '<power-build>', 'exec'), namespace)
        with tempfile.TemporaryDirectory() as directory, patch.object(namespace['cockpit'], 'build', return_value={}):
            with self.assertRaisesRegex(RuntimeError, 'both required executable binaries'):
                namespace['build']({}, Path(directory), 'a' * 32)

    def test_publication_refuses_unverified_cleanup(self):
        with self.assertRaisesRegex(RuntimeError, 'independent cleanup'):
            power.evidence({'observations': {}, 'cleaned': False})

    def test_publication_omits_sensitive_baseline_and_retains_actual_snapshot_hashes(self):
        baseline = {'instances': {'original': {'status': 'Running', 'config': {'secret': 'never-publish-baseline'}}}}
        cleanup = {name: True for name in ('baseline_configuration_restored', 'lab_resources_absent',
                                         'transient_units_absent', 'power_probe_unit_absent')}
        cleanup['baseline_after'] = baseline
        state = {'run_id': 'a' * 32, 'cleaned': True, 'started_at_unix': 1,
                 'baseline': baseline, 'observations': {'cleanup': cleanup},
                 'source_sha256': {'crates/virtualization/src/proxmox.rs': 'tested-snapshot-not-current-checkout'}}
        summary = power.evidence(state)
        self.assertNotIn('never-publish-baseline', json.dumps(summary))
        self.assertEqual(summary['source_sha256'], state['source_sha256'])
        self.assertEqual(summary['original_vm_power_states'], {'original': 'Running'})

    def test_existing_manifest_cannot_recreate_resources(self):
        with tempfile.TemporaryDirectory() as directory:
            filename = Path(directory) / 'manifest.json'
            filename.write_text('{}')
            with self.assertRaisesRegex(RuntimeError, 'cleanup-only'):
                power.execute(SimpleNamespace(action='run', state=str(filename)))

    def test_seed_checks_both_owned_vm_descriptions_before_creating_token(self):
        namespace = {'__name__': 'power_seed_test'}
        exec(compile(power.wrapped('proxmox_cockpit_guest', 'proxmox_power_guest.py'), '<power-guest>', 'exec'), namespace)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'run-id').write_text('a' * 32)
            calls = []

            def run(*arguments):
                calls.append(arguments)
                if arguments[0] == 'hostname':
                    return 'hc-pve-' + 'a' * 10
                return 'description: Foreign VM'

            with patch.dict(namespace, {'Path': lambda _: root}), patch.object(namespace['cockpit'], 'run', side_effect=run), patch.object(namespace['sys'], 'argv', ['probe', 'power-seed', 'a' * 32]):
                with self.assertRaisesRegex(RuntimeError, 'Foreign nested VM'):
                    namespace['main']()
            self.assertFalse(any(arguments[0] == 'pveum' for arguments in calls))

    def test_seed_power_acl_is_vm_scoped_and_token_value_is_never_printed(self):
        namespace = {'__name__': 'power_seed_test'}
        exec(compile(power.wrapped('proxmox_cockpit_guest', 'proxmox_power_guest.py'), '<power-guest>', 'exec'), namespace)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'run-id').write_text('a' * 32)
            calls = []

            def run(*arguments):
                calls.append(arguments)
                if arguments[0] == 'hostname':
                    return 'hc-pve-' + 'a' * 10
                if arguments[:2] == ('qm', 'config'):
                    return 'description: Owned disposable run ' + 'a' * 32
                if arguments[:4] == ('pveum', 'user', 'token', 'add'):
                    return json.dumps({'full-tokenid': 'hc-power@pve!power', 'value': 'secret-not-for-evidence'})
                if arguments[:4] == ('pveum', 'user', 'token', 'permissions'):
                    return '{}'
                return ''

            output = io.StringIO()
            with patch.dict(namespace, {'Path': lambda _: root}), patch.object(namespace['cockpit'], 'run', side_effect=run), patch.object(namespace['sys'], 'argv', ['probe', 'power-seed', 'a' * 32]), contextlib.redirect_stdout(output):
                namespace['main']()
            role = 'HCPower' + 'a' * 10
            self.assertEqual([arguments[3] for arguments in calls if arguments[:3] == ('pveum', 'acl', 'modify') and role in arguments], ['/vms/7101', '/vms/7101'])
            self.assertNotIn('secret-not-for-evidence', output.getvalue())
            self.assertEqual((root / 'power-token.json').stat().st_mode & 0o777, 0o600)


if __name__ == '__main__':
    unittest.main()
