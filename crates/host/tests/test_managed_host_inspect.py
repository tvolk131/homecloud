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
