"""The existing privileged adapter dispatches storage inspection to Rust."""
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('storage_dispatch_host', Path(__file__).parents[1] / 'host_runtime.py')
host = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = host
spec.loader.exec_module(host)


class StorageDispatch(unittest.TestCase):
    def request(self, run):
        with tempfile.TemporaryDirectory() as temporary:
            request = {'kind': 'storage', 'command': 'untrusted', 'path': '/tmp/untrusted'}
            with patch.multiple(host, STATE=Path(temporary), load_config=Mock(return_value={'pool': 'tank'}), run=run), \
                    patch.object(host.os, 'geteuid', return_value=0), \
                    patch.object(sys, 'argv', ['host_runtime.py']), \
                    patch.object(sys, 'stdin', io.StringIO(json.dumps(request))):
                return host.main()

    def test_storage_only_executes_the_fixed_installed_rust_command(self):
        inventory = {'disks': [], 'pools': [], 'disk_collection': {'error': 'fixture unavailable'}}
        run = Mock(return_value=SimpleNamespace(stdout=json.dumps(inventory)))
        self.assertEqual(self.request(run), {'kind': 'storage', 'data': inventory})
        run.assert_called_once_with('/usr/local/bin/homecloudd', 'collect-storage', timeout=120)

    def test_missing_or_failed_collector_is_not_successful_empty_inventory(self):
        for error in [FileNotFoundError('missing collector'), RuntimeError('collector failed')]:
            with self.subTest(error=error):
                with self.assertRaises(type(error)):
                    self.request(Mock(side_effect=error))

    def test_malformed_collector_output_is_not_successful_empty_inventory(self):
        with self.assertRaises(json.JSONDecodeError):
            self.request(Mock(return_value=SimpleNamespace(stdout='not json')))


if __name__ == '__main__':
    unittest.main()
