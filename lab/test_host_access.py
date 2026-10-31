import contextlib
import io
import json
import os
from pathlib import Path
import runpy
import tempfile
import types
import unittest
from unittest.mock import patch


LAB = Path(__file__).resolve().parent


class AccessPathsTests(unittest.TestCase):
    def test_default_is_independent_of_checkout(self):
        with patch.dict(os.environ, {}, clear=True):
            values = runpy.run_path(str(LAB / 'remote.py'))
        expected = Path.home() / '.config/homecloud/host-access/resolvr-box/ssh_config'
        self.assertEqual(values['SSH_CONFIG'], expected.resolve())
        self.assertEqual(values['ACCESS'], expected.parent.resolve())
        self.assertEqual(values['SSH'][1:3], ['-F', str(expected.resolve())])

    def test_explicit_config_override_expands_home(self):
        with patch.dict(os.environ, {'HOMECLOUD_SSH_CONFIG': '~/private lab/custom.conf'}):
            values = runpy.run_path(str(LAB / 'remote.py'))
        expected = (Path.home() / 'private lab/custom.conf').resolve()
        self.assertEqual(values['SSH_CONFIG'], expected)
        self.assertEqual(values['ACCESS'], expected.parent)


class EvidenceAccessTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / 'lab/evidence').mkdir(parents=True)
        self.access = self.root / 'private access'
        self.access.mkdir(mode=0o700)
        self.config = self.access / 'ssh_config'
        self.original = 'Host hclab-host\n    HostName pinned.example\n    HostKeyAlias original-pin\n'
        self.config.write_text(self.original)
        self.known = self.access / 'known_hosts'
        self.known.write_text('original-pin ssh-ed25519 original-key\n')
        self.ssh = ['/usr/bin/ssh', '-F', str(self.config), '-o', 'BatchMode=yes', 'hclab-host']
        self.remote = types.SimpleNamespace(ACCESS=self.access, ROOT=self.root,
                                           SSH_CONFIG=self.config, SSH=self.ssh)
        self.report = {'failures': [], 'ssh': {'hclab-vm01': {
            'vm_uuid': 'test-guest', 'host_public_key': 'ssh-ed25519 guest-key',
            'address': '10.77.1.10'}}}

    def fetch(self):
        def response(command, **kwargs):
            document = self.report if command[-1].endswith('/verification.json') else {}
            return types.SimpleNamespace(stdout=json.dumps(document))
        with patch.dict('sys.modules', {'remote': self.remote}), \
                patch('subprocess.run', side_effect=response) as run, \
                contextlib.redirect_stdout(io.StringIO()):
            runpy.run_path(str(LAB / 'fetch_evidence.py'))
        return run

    def test_fetch_preserves_management_config_and_generates_only_guest_entries(self):
        run = self.fetch()
        for call in run.call_args_list:
            self.assertEqual(call.args[0][:-1], self.ssh)
        self.assertEqual(self.config.read_text(), self.original)
        generated = self.access / 'lab_guests.conf'
        text = generated.read_text()
        self.assertIn('Host hclab-vm01', text)
        self.assertNotIn('Host hclab-host', text)
        self.assertIn(f'ProxyCommand /usr/bin/ssh -F "{self.config}"', text)
        self.assertIn('homecloud-test-guest ssh-ed25519 guest-key', self.known.read_text())
        self.assertEqual(generated.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.known.stat().st_mode & 0o777, 0o600)

    def test_changed_guest_pin_is_rejected_without_rewriting_access(self):
        self.known.write_text('homecloud-test-guest ssh-ed25519 unexpected-key\n')
        original = self.known.read_text()
        with self.assertRaisesRegex(AssertionError, 'Guest host key changed'):
            self.fetch()
        self.assertEqual(self.config.read_text(), self.original)
        self.assertEqual(self.known.read_text(), original)
        self.assertFalse((self.access / 'lab_guests.conf').exists())


if __name__ == '__main__':
    unittest.main()
