"""Provider bootstrap contracts; no downloads, login, or model requests."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('setup_providers', Path(__file__).parents[3]/'guest/setup-providers.py')
providers = importlib.util.module_from_spec(spec);spec.loader.exec_module(providers)

class ProviderTests(unittest.TestCase):
    def test_download_rejects_changed_artifact_and_removes_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            output=Path(tmp)/'artifact'
            with patch.object(providers.urllib.request,'urlopen',return_value=io.BytesIO(b'wrong package')):
                with self.assertRaisesRegex(RuntimeError,'checksum'):providers.download('https://example.com/release',output,'0'*64)
            self.assertFalse(output.exists())

    def test_existing_compatible_installations_are_retained_without_login_or_download(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(providers.os,'getuid',return_value=1000), patch.object(providers.platform,'system',return_value='Linux'), patch.object(providers.platform,'machine',return_value='x86_64'), patch.object(providers.Path,'home',return_value=Path(tmp)), patch.object(providers.shutil,'which',side_effect=lambda name:'/existing/'+name), patch.object(providers,'version',side_effect=[(0,165,0),(2,1,308)]), patch.object(providers,'download') as download:
            providers.ensure_providers();download.assert_not_called()

    def test_broken_existing_installation_is_not_silently_replaced(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(providers.os,'getuid',return_value=1000), patch.object(providers.platform,'system',return_value='Linux'), patch.object(providers.platform,'machine',return_value='x86_64'), patch.object(providers.Path,'home',return_value=Path(tmp)), patch.object(providers.shutil,'which',return_value='/existing/codex'), patch.object(providers,'version',return_value=(0,100,0)), patch.object(providers,'download') as download:
            with self.assertRaisesRegex(RuntimeError,'unsupported'):providers.ensure_providers()
            download.assert_not_called()

    @unittest.skipUnless(hasattr(tarfile,'data_filter'),'Extraction is exercised with modern Python on the Ubuntu guest')
    def test_package_checks_manifest_and_required_code_mode_sidecars(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);archive=root/'package.tgz';dest=root/'unpacked';dest.mkdir()
            manifest=json.dumps({'version':'0.165.0','target':'x86_64-unknown-linux-musl','entrypoint':'bin/codex'}).encode()
            with tarfile.open(archive,'w:gz') as tar:
                for name,data in [('codex-package.json',manifest),('bin/codex',b'cli'),('bin/codex-code-mode-host',b'host'),('codex-path/rg',b'rg')]:
                    member=tarfile.TarInfo(name);member.size=len(data);member.mode=0o755;tar.addfile(member,io.BytesIO(data))
            providers.unpack_codex(archive,dest,'0.165.0')
            self.assertTrue((dest/'bin/codex-code-mode-host').exists())
            with self.assertRaisesRegex(RuntimeError,'manifest'):providers.unpack_codex(archive,dest,'0.100.0')

    @unittest.skipUnless(hasattr(tarfile,'data_filter'),'Extraction is exercised with modern Python on the Ubuntu guest')
    def test_package_cannot_escape_staging_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);archive=root/'package.tgz';dest=root/'unpacked';dest.mkdir()
            with tarfile.open(archive,'w:gz') as tar:
                member=tarfile.TarInfo('../escaped');member.size=1;tar.addfile(member,io.BytesIO(b'x'))
            with self.assertRaises(tarfile.FilterError):providers.unpack_codex(archive,dest,'0.165.0')
            self.assertFalse((root/'escaped').exists())

    def check_service_activation(self, active):
        import os
        import subprocess
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);home=root/'home';binary=home/'.local/bin';binary.mkdir(parents=True)
            fake=root/'commands';fake.mkdir();calls=root/'calls'
            scripts={
                binary/'t3': '#!/bin/sh\nif [ "$1" = --version ]; then echo "t3 v0.1.2"; else echo "$*" >> "$TEST_CALLS"; fi\n',
                fake/'id': '#!/bin/sh\necho 1000\n',
                fake/'python3': '#!/bin/sh\necho provider-setup >> "$TEST_CALLS"\n',
                fake/'curl': '#!/bin/sh\nexit 99\n',
                fake/'systemctl': '#!/bin/sh\nif [ "$2" = is-active ]; then exit "$TEST_ACTIVE"; fi\necho "$*" >> "$TEST_CALLS"\n',
            }
            for path,text in scripts.items():path.write_text(text);path.chmod(0o755)
            credentials=home/'.t3/userdata/owner-sentinel';credentials.parent.mkdir(parents=True);credentials.write_text('preserve owner state')
            envfile=home/'.t3/service.env';envfile.write_text('T3CODE_HOST=127.0.0.1\nT3CODE_PORT=3773\n')
            env=dict(os.environ,HOME=str(home),PATH=str(fake)+os.pathsep+os.environ['PATH'],TEST_CALLS=str(calls),TEST_ACTIVE='0' if active else '3')
            script=Path(__file__).parents[3]/'guest/setup-t3.sh'
            result=subprocess.run(['/bin/sh',str(script),'0.1.2'],env=env,text=True,capture_output=True)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(credentials.read_text(),'preserve owner state')
            self.assertEqual(envfile.read_text(),'T3CODE_HOST=127.0.0.1\nT3CODE_PORT=3773\n')
            self.assertIn(str(binary),(home/'.config/systemd/user/t3code.service.d/homecloud.conf').read_text())
            events=calls.read_text().splitlines()
            self.assertIn('provider-setup',events)
            self.assertEqual(events.count('service install'),1)
            self.assertEqual(events.count('service restart'),1 if active else 0)

    def test_repair_restarts_existing_service_to_activate_provider_path(self):
        self.check_service_activation(True)

    def test_initial_service_install_starts_once_and_preserves_owner_state(self):
        self.check_service_activation(False)
