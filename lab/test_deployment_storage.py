import copy
import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location('deployment_guest', Path(__file__).with_name('deployment_guest.py'))
guest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guest)


class StorageCheckpointTests(unittest.TestCase):
    def setUp(self):
        observation = {'observed_at': 100, 'error': None}
        smart = {'state': 'unavailable', 'passed': None, 'collection': {'error': 'smartctl is missing'}}
        self.snapshot = {'me': {'role': 'admin'}, 'inventory': {'storage': {
            'disk_collection': copy.deepcopy(observation), 'pool_collection': copy.deepcopy(observation),
            'disks': [
                {'id': 'boot', 'path': '/dev/sda', 'devices': [{'mountpoints': ['/']}], 'smart': copy.deepcopy(smart)},
                {'id': 'pool', 'path': '/dev/sdb', 'devices': [], 'smart': copy.deepcopy(smart)}],
            'pools': [{'name': 'hcnested', 'state': 'ONLINE', 'managed': True, 'vdevs': [
                {'children': [{'children': [], 'path': '/dev/sdb1', 'disk_id': 'pool'}]}]}]}}}

    def check(self):
        return guest.storage_summary(self.snapshot, 90, 110, False)

    def test_nested_pool_links_and_honest_smart_unavailability_pass(self):
        summary = self.check()
        self.assertEqual(summary['disk_count'], 2)
        self.assertEqual(summary['boot_disks'], ['/dev/sda'])
        self.assertEqual(summary['linked_vdev_leaves'], 1)
        self.assertFalse(summary['smartctl_available'])

    def test_missing_or_member_inventory_is_rejected(self):
        self.snapshot['me']['role'] = 'member'
        with self.assertRaisesRegex(AssertionError, 'administrator'):
            self.check()
        self.snapshot['me']['role'] = 'admin'
        self.snapshot['inventory']['storage'] = None
        with self.assertRaisesRegex(AssertionError, 'absent'):
            self.check()

    def test_pre_upgrade_stale_future_and_failed_observations_are_rejected(self):
        observation = self.snapshot['inventory']['storage']['pool_collection']
        for timestamp in (89, 111):
            observation['observed_at'] = timestamp
            with self.assertRaisesRegex(AssertionError, 'not fresh'):
                self.check()
        observation.update(observed_at=100, error='zpool failed')
        with self.assertRaisesRegex(AssertionError, 'zpool failed'):
            self.check()

    def test_unmatched_pool_leaf_is_rejected(self):
        self.snapshot['inventory']['storage']['pools'][0]['vdevs'][0]['children'][0]['disk_id'] = None
        with self.assertRaisesRegex(AssertionError, 'not linked'):
            self.check()

    def test_missing_boot_disk_is_rejected(self):
        self.snapshot['inventory']['storage']['disks'][0]['devices'] = []
        with self.assertRaisesRegex(AssertionError, 'Boot mount'):
            self.check()

    def test_invented_smart_health_is_rejected(self):
        self.snapshot['inventory']['storage']['disks'][0]['smart']['passed'] = True
        with self.assertRaisesRegex(AssertionError, 'must not imply healthy'):
            self.check()


if __name__ == '__main__':
    unittest.main()
