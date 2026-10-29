"""Real adapter algorithms driven by explicit ARC effects and durable journals."""
import copy
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock,patch

ROOT=Path(__file__).resolve().parents[3]
sys.path.insert(0,str(ROOT/'crates/host'));sys.path.insert(0,str(ROOT/'lab/models'))
import arc_policy as arc
import memory_capacity as memory
from arc import ArcModel,GIB

class MemoryTests(unittest.TestCase):
    def setUp(self):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);self.root=Path(t.name)
        self.config=self.root/'conf/zz-homecloud-arc.conf';self.config.parent.mkdir()
        self.model=ArcModel();self.host=SimpleNamespace(STATE=self.root,run=self.model.run)
        self.base={'capacity':{'memory_bytes':12*GIB},'minimum_reserves':{'memory_bytes':4*GIB},'memory_operating_floor_bytes':2*GIB,'arc_recommended_maximum_bytes':2*GIB}
        for key,value in [('CONFIG',self.config),('PARAMS',self.model)]:
            p=patch.object(arc,key,value);p.start();self.addCleanup(p.stop)
        for key,value in [('observe',self.model.observe),('physical_memory',lambda:self.model.physical)]:
            p=patch.object(arc,key,value);p.start();self.addCleanup(p.stop)
    def test_effective_min_max_order_and_reboot_persistence(self):
        self.model.minimum=3*GIB//2
        arc.apply(self.host,self.base,{'mode':'custom','max_bytes':GIB})
        writes=[e for e in self.model.events if e[0].startswith('zfs_arc')]
        self.assertEqual(writes,[('zfs_arc_min',GIB//2),('zfs_arc_max',GIB)])
        self.model.resize_boot(48*GIB,GIB,GIB//2)
        self.assertTrue(arc.enforced({'mode':'custom','max_bytes':GIB},self.base))
    def test_ignored_minimum_never_advances_to_maximum_write(self):
        self.model.minimum=3*GIB//2;self.model.ignore_minimum=True
        with self.assertRaisesRegex(AssertionError,'coordinated ARC minimum'):
            arc.apply(self.host,self.base,{'mode':'custom','max_bytes':GIB})
        self.assertFalse(any(e[0]=='zfs_arc_max' for e in self.model.events))
    def test_rejected_maximum_cannot_be_confirmed_by_echoed_sysfs(self):
        self.model.minimum=3*GIB//2
        (self.model/'zfs_arc_max').write_text(str(GIB))
        self.assertEqual(self.model.parameter,GIB)
        self.assertEqual(self.model.maximum,2*GIB)
        self.assertFalse(arc.enforced({'mode':'custom','max_bytes':GIB},self.base))
    def test_ram_shrink_is_observation_not_loss_of_reviewed_intent(self):
        self.model.resize_boot(8*GIB,2*GIB)
        base=memory.overlay(self.host,self.base)
        p=patch.object(memory,'status',wraps=memory.status)
        # Linux boot IDs are supplied explicitly on non-Linux test runners.
        with patch.object(Path,'read_text',return_value='00000000-0000-0000-0000-000000000001'):
            proof=memory.status(base)
        self.assertEqual(proof['safe_bytes'],4*GIB)
        self.assertEqual(proof['reviewed_bytes'],12*GIB)
        self.assertEqual(base['capacity']['memory_bytes'],12*GIB)
    def test_cancel_tombstone_fences_a_delayed_capacity_apply(self):
        host=SimpleNamespace(STATE=self.root)
        def save(path,value):path.write_text(json.dumps(value));path.chmod(0o600)
        host.save=save
        initial={'revision':0,'bytes':12*GIB,'operations':{}}
        def record(*args):
            p=self.root/'memory-capacity.json'
            return json.loads(p.read_text()) if p.exists() else copy.deepcopy(initial)
        op={'id':'00000000-0000-4000-8000-000000000001','expected_revision':0,'target_bytes':10*GIB}
        config={'_reserve_base':self.base}
        with patch.object(memory,'record',side_effect=record),patch.object(memory,'status',return_value={'revision':0}):
            result=memory.cancel(host,config,op)
            self.assertEqual(result['operation_id'],op['id'])
            with self.assertRaisesRegex(AssertionError,'cancelled'):memory.adopt(host,config,op)
        self.assertTrue(record()['operations'][op['id']]['cancelled'])

    def test_model_matches_the_recorded_real_2_4_1_minimum_and_coordinated_change(self):
        evidence=json.loads((ROOT/'lab/evidence/managed-host-memory.json').read_text())['stages']
        grown=next(v for k,v in evidence.items() if k.startswith('outer-36-'))
        actual=evidence['coordinated-arc'];model=ArcModel(physical=grown['memory'],cap=2*GIB)
        self.assertEqual(model.minimum,actual['before']['c_min'])
        with patch.object(arc,'PARAMS',model),patch.object(arc,'observe',side_effect=model.observe),patch.object(arc,'physical_memory',return_value=model.physical):
            arc.apply(SimpleNamespace(run=model.run),self.base,{'mode':'custom','max_bytes':GIB})
        self.assertEqual((model.minimum,model.maximum),(actual['after']['c_min'],actual['after']['c_max']))
        self.assertEqual(model.minimum_parameter,int(actual['parameters']['zfs_arc_min']))
