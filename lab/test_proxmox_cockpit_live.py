"""Offline safety and failure-path checks for disposable real-PVE validation."""

import argparse
import ast
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import proxmox_cockpit_live as live


class CockpitLiveTests(unittest.TestCase):
    def host_functions(self):
        tree = ast.parse(Path(__file__).with_name('proxmox_cockpit_host.py').read_text())
        namespace = {}
        exec(compile(tree, '<cockpit-host>', 'exec'), namespace)
        return namespace

    def test_sources_parse(self):
        for name in ('proxmox_cockpit_host.py', 'proxmox_cockpit_guest.py'):
            ast.parse(Path(__file__).with_name(name).read_text())

    def test_identity_rejected_before_host_commands(self):
        namespace = self.host_functions()
        with patch('subprocess.run') as command:
            for token in ('../escape', 'g' * 32):
                with self.assertRaisesRegex(RuntimeError, 'Invalid run identity'):
                    namespace['identities']({'run_id': token})
            with self.assertRaisesRegex(RuntimeError, 'Resource identity mismatch'):
                namespace['identities']({'run_id': 'a' * 32})
            command.assert_not_called()

    def test_foreign_or_symlink_build_directory_is_never_owned(self):
        namespace = self.host_functions()
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / 'owned'
            directory.mkdir()
            (directory / 'run-id').write_text('foreign')
            with self.assertRaisesRegex(RuntimeError, 'Foreign build'):
                namespace['owned'](directory, 'a' * 32)
            linked = Path(temporary) / 'linked'
            linked.symlink_to(directory)
            with self.assertRaisesRegex(RuntimeError, 'unsafe build'):
                namespace['owned'](linked, 'foreign')

    def test_failed_prepare_still_cleans_after_build(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = argparse.Namespace(state=str(Path(temporary) / 'state.json'), action='run')
            actions = []

            def record(arguments, state, action):
                actions.append(action)
                if action == 'prepare':
                    raise RuntimeError('fixture prepare failure')
                return {}

            with patch.object(live, 'helper', return_value={'controller': 'active'}):
                with patch.object(live, 'record', side_effect=record):
                    with self.assertRaisesRegex(RuntimeError, 'fixture prepare failure'):
                        live.execute(args)
            state = json.loads(Path(args.state).read_text())
            self.assertTrue(state['cleaned'])
            self.assertEqual(actions[-1], 'cleanup')
            self.assertIn('build', actions)

    def test_existing_manifest_is_cleanup_only(self):
        with tempfile.TemporaryDirectory() as temporary:
            filename = Path(temporary) / 'state.json'
            filename.write_text('{}')
            with self.assertRaisesRegex(RuntimeError, 'cleanup-only'):
                live.execute(argparse.Namespace(state=str(filename), action='run'))

    def test_assessment_never_accepts_mapper_or_tls_failure(self):
        cases = {name: {'ok': False, 'error': 'injected failure'} for name in
                 ('reader', 'trusted-ca', 'trusted-ca-bad-fingerprint', 'iroh', 'iroh-trusted-ca',
                  'noaccess', 'invalid', 'bad-fingerprint', 'untrusted-ca', 'public-secret')}
        cases['bad-fingerprint'] = {'ok': True}
        assessment = live.assess({'cases': cases, 'reference_resources': []})
        self.assertFalse(assessment['passed'])
        self.assertIn('reader: injected failure', assessment['failures'])
        self.assertIn('bad-fingerprint: unexpectedly accepted', assessment['failures'])

    def test_assessment_checks_native_capacity_and_transport_mapping(self):
        inventory = {
            'source': 'proxmox',
            'nodes': [{'name': 'node', 'cpu_threads': 4, 'memory_capacity_bytes': 12 * 1024**3}],
            'storage': [{'node': 'node', 'name': 'test', 'capacity_bytes': 32 * 1024**3, 'status': 'available'}],
            'guests': [{
                'vmid': vmid, 'kind': 'virtual_machine', 'template': template, 'status': 'stopped',
                'cpu_limit': 2, 'memory_limit_bytes': 512 * 1024**2,
                'reported_disk_capacity_bytes': 1024**3,
            } for vmid, template in ((7101, False), (7199, True))],
        }
        cases = {name: {'ok': False, 'error': error} for name, error in (
            ('invalid', 'authentication failed'), ('bad-fingerprint', 'connection failed'),
            ('untrusted-ca', 'connection failed'), ('public-secret', 'private regular file'),
        )}
        cases['reader'] = {'ok': True, 'result': {'inventory': inventory}}
        cases['trusted-ca'] = {'ok': True, 'result': {'inventory': inventory}}
        cases['trusted-ca-bad-fingerprint'] = cases['trusted-ca']
        cases['noaccess'] = {'ok': True, 'result': {'inventory': {'nodes': [], 'guests': [], 'storage': []}}}
        cases['iroh'] = {'ok': True, 'result': {
            'inventory': inventory, 'authenticated_iroh': True, 'member_denied': True, 'unknown_denied': True,
            'revoked_denied': True, 'legacy_denied': True, 'legacy_vms_created': 0,
        }}
        cases['iroh-trusted-ca'] = cases['iroh']
        result = {'cases': cases, 'probe_uid': 1000, 'reference_resources': [
            {'type': 'node', 'node': 'node', 'maxcpu': 4, 'maxmem': 12 * 1024**3},
            {'type': 'storage', 'node': 'node', 'storage': 'test', 'maxdisk': 32 * 1024**3, 'status': 'available'},
            {'type': 'qemu', 'node': 'node', 'vmid': 7101},
            {'type': 'qemu', 'node': 'node', 'vmid': 7199, 'template': 1},
        ]}
        self.assertTrue(live.assess(result)['passed'])
        result['reference_resources'][0]['maxmem'] = 0
        self.assertIn('Incorrect node field: memory_capacity_bytes', live.assess(result)['failures'])
        result['reference_resources'][0]['maxmem'] = 12 * 1024**3
        result['reference_resources'][-1]['template'] = 0
        inventory['guests'][-1]['template'] = False
        assessment = live.assess(result)
        self.assertIn('Native PVE template cache not settled for seeded VM 7199', assessment['failures'])
        self.assertFalse(any('Template mapping differs' in failure for failure in assessment['failures']))

    def test_source_snapshot_pins_client_and_excludes_credentials(self):
        archive, hashes = live.source_archive()
        self.assertTrue(archive)
        self.assertIn('crates/virtualization/src/proxmox.rs', hashes)
        self.assertIn('Cargo.lock', hashes)
        self.assertTrue(all(name.startswith('crates/') or name in ('Cargo.toml', 'Cargo.lock') for name in hashes))

    def test_evidence_refuses_unverified_cleanup(self):
        with self.assertRaisesRegex(RuntimeError, 'exact cleanup verification'):
            live.evidence({'cleaned': False, 'observations': {}})

    def test_cache_churn_does_not_authorize_changing_nonowned_storage(self):
        namespace = self.host_functions()
        previous = 'a' * 64
        current = 'b' * 64
        before = {'datasets': ['hclab/incus/images/' + previous], 'controller': 'active'}
        after = {'datasets': ['hclab/incus/images/' + current], 'controller': 'active'}
        images = [{'fingerprint': current, 'auto_update': True}]
        comparison = namespace['compare_baseline'](before, after, images)
        self.assertTrue(comparison['baseline_configuration_restored'])
        self.assertFalse(comparison['baseline_exactly_restored'])
        self.assertEqual(comparison['unexpected_fields'], [])
        self.assertIsNotNone(comparison['unrelated_image_cache_churn'])
        after['datasets'].append('hclab/nonowned-data')
        self.assertEqual(namespace['compare_baseline'](before, after, images)['unexpected_fields'], ['datasets'])
        after['datasets'].pop()
        after['controller'] = 'inactive'
        self.assertEqual(namespace['compare_baseline'](before, after, images)['unexpected_fields'], ['controller'])

    def test_seed_waits_for_template_not_just_capacity(self):
        namespace = {'__name__': 'cockpit_guest_test'}
        source = Path(__file__).with_name('proxmox_cockpit_guest.py').read_text()
        exec(compile(source, '<cockpit-guest>', 'exec'), namespace)
        resources = [{
            'vmid': vmid, 'maxcpu': 2, 'maxmem': 512 * 1024**2, 'maxdisk': 1024**3, 'template': 0,
        } for vmid in (7101, 7199)]
        self.assertFalse(namespace['settled_inventory'](resources))
        resources[1]['template'] = 1
        self.assertTrue(namespace['settled_inventory'](resources))
        resources[1]['maxdisk'] = 0
        self.assertFalse(namespace['settled_inventory'](resources))


if __name__ == '__main__':
    unittest.main()
