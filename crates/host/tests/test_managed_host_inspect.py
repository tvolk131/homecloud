"""Inspect reports blocked memory admission without repairing guest services."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[3]
spec=importlib.util.spec_from_file_location('managed_host_guest_inspection',ROOT/'guest/managed-host.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class InspectionTests(unittest.TestCase):
    def test_running_services_with_arc_drift_are_unavailable_and_inspect_has_no_repair_effect(self):
        with tempfile.TemporaryDirectory() as t:
            state=Path(t);(state/'application.json').write_text(json.dumps({'installed':True}))
            calls=[];ready=False
            def run(*args,**kwargs):
                calls.append(args)
                if args[0]=='systemctl':self.assertEqual(args[1],'is-active');return 'active'
                if args[0]=='zpool':self.assertEqual(args[1],'list');return 'ONLINE'
                self.assertEqual(args,('python3','/usr/local/libexec/homecloud/host_runtime.py'))
                self.assertEqual(json.loads(kwargs['input']),{'kind':'reserve_status'})
                return json.dumps({'data':{'memory':{'arc_ready':ready},'applying':not ready}})
            with patch.object(m,'STATE',state),patch.object(m,'run',side_effect=run):
                blocked=m.inspect();self.assertEqual(blocked['observation']['service'],'unavailable')
                self.assertFalse(next(c for c in blocked['observation']['components'] if c['name']=='Memory policy')['ready'])
                ready=True;healthy=m.inspect();self.assertEqual(healthy['observation']['service'],'running')
            self.assertEqual(len(calls),8)


class UpgradeTests(unittest.TestCase):
    def test_existing_host_upgrade_never_installs_packages_or_reapplies_pool_policy(self):
        import hashlib,io,tarfile
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);policy=root/'policy.json';policy.write_text('{}');receipt=root/'receipt.json';receipt.write_text('{}')
            bundle=root/'bundle.tgz'
            with tarfile.open(bundle,'w:gz') as archive:
                data=b'# verified fixture';entry=tarfile.TarInfo('scripts/host_deploy.py');entry.size=len(data);archive.addfile(entry,io.BytesIO(data))
            sha=hashlib.sha256(bundle.read_bytes()).hexdigest();calls=[]
            # macOS's system Python predates extraction filters. This fixture
            # contains one known regular file; Linux production uses filter=data.
            extract=tarfile.TarFile.extractall
            def fixture_extract(archive,path,**kwargs):
                import inspect
                return extract(archive,path,**kwargs) if 'filter' in inspect.signature(extract).parameters else extract(archive,path)
            def run(*args,**kwargs):calls.append(args);return ''
            with patch.object(m,'STATE',root/'state'),patch.object(m,'ROOT',root/'source'),patch.object(m,'POLICY',policy),patch.object(m,'RECEIPT',receipt),patch.object(m,'BUNDLE',bundle),patch.object(m,'run',side_effect=run),patch.object(tarfile.TarFile,'extractall',fixture_extract):
                m.install('00000000-0000-4000-8000-000000000001',sha,'0x5000000000000001')
                self.assertEqual(json.loads((m.STATE/'application.json').read_text())['job']['phase'],'succeeded')
            self.assertEqual(calls,[('python3','/usr/local/libexec/homecloud-deploy.py','upgrade','--bundle',str(bundle),'--sha256',sha)])
    def test_legacy_host_is_rejected_before_package_or_storage_effects(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);policy=root/'policy.json';policy.write_text('{}');calls=[]
            with patch.object(m,'STATE',root/'state'),patch.object(m,'POLICY',policy),patch.object(m,'RECEIPT',root/'absent'),patch.object(m,'run',side_effect=lambda *a,**k:calls.append(a)):
                with self.assertRaisesRegex(AssertionError,'adoption'):m.install('00000000-0000-4000-8000-000000000001','0'*64,'0x5000000000000001')
            self.assertEqual(calls,[])
