"""ARC behavior with disposable sysfs/config files; no host memory is changed."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import Mock,patch

spec=importlib.util.spec_from_file_location('arc_test_policy',Path(__file__).parents[1]/'arc_policy.py')
a=importlib.util.module_from_spec(spec);spec.loader.exec_module(a)
GIB=1024**3

class ArcPolicyTests(unittest.TestCase):
    def setUp(self):
        temp=tempfile.TemporaryDirectory();self.addCleanup(temp.cleanup);self.root=Path(temp.name)
        self.params=self.root/'params';self.params.mkdir();(self.params/'zfs_arc_max').write_text(str(2*GIB))
        self.config=self.root/'conf'/'zz-homecloud-arc.conf';self.config.parent.mkdir()
        self.stats=self.root/'arcstats';self.stats.write_text('header\nheader\nsize 4 '+str(3*GIB)+'\nc_min 4 '+str(GIB//2)+'\nc_max 4 '+str(2*GIB)+'\n')
        self.version=self.root/'version';self.version.write_text('2.4.1-ubuntu')
        for key,value in [('PARAMS',self.params),('CONFIG',self.config),('STATS',self.stats),('VERSION',self.version)]:
            patcher=patch.object(a,key,value);patcher.start();self.addCleanup(patcher.stop)
        patcher=patch.object(a,'physical_memory',return_value=16*GIB);patcher.start();self.addCleanup(patcher.stop)
        self.host=SimpleNamespace(run=Mock())
        self.profile={"minimum_reserves":{"memory_bytes":4*GIB},"memory_operating_floor_bytes":2*GIB,"arc_recommended_maximum_bytes":2*GIB}
        self.mode={'mode':'custom','max_bytes':GIB}
    def observed(self,maximum):
        return dict(a.observe(),maximum_bytes=maximum)
    def test_telemetry_and_supported_native_defaults(self):
        observed=a.observe();self.assertEqual(observed['maximum_bytes'],2*GIB)
        self.assertEqual(observed['automatic_maximum_bytes'],15*GIB)
        self.assertEqual(a.automatic_maximum('2.2.8',16*GIB),8*GIB)
        self.assertIsNone(a.automatic_maximum('9.0.0',16*GIB))
    def test_lower_limit_is_confirmed_even_while_cache_usage_is_above_it(self):
        before=a.observe();after=dict(before,maximum_bytes=GIB)
        with patch.object(a,'observe',side_effect=[before,before,after]):a.apply(self.host,self.profile,self.mode)
        self.assertEqual((self.params/'zfs_arc_max').read_text(),str(GIB))
        self.assertIn('zfs_arc_max='+str(GIB),self.config.read_text())
        self.host.run.assert_called_once_with('update-initramfs','-u','-k','all',timeout=180)
    def test_ignored_kernel_write_is_not_success(self):
        with self.assertRaisesRegex(AssertionError,'did not apply'):a.apply(self.host,self.profile,self.mode)
        self.assertEqual(a.observe()['maximum_bytes'],2*GIB)
    def test_automatic_stays_in_its_budget_and_persists_a_numeric_cap(self):
        before=a.observe();after=dict(before,maximum_bytes=2*GIB)
        with patch.object(a,'observe',side_effect=[before,before,after]):a.apply(self.host,self.profile, {'mode':'automatic'})
        self.assertEqual((self.params/'zfs_arc_max').read_text(),str(2*GIB))
        self.assertIn('zfs_arc_max='+str(2*GIB),self.config.read_text())
        self.host.run.reset_mock();(self.params/'zfs_arc_max').write_text('0')
        with patch.object(a,'observe',return_value=dict(after,parameter_bytes=0)):
            a.apply(self.host,self.profile, {'mode':'automatic'},persist=False)
        self.host.run.assert_not_called()
        self.assertEqual((self.params/'zfs_arc_max').read_text(),str(2*GIB))
    def test_minimum_zero_and_unbacked_custom_are_rejected_before_effects(self):
        for mode in [{'mode':'custom','max_bytes':0},{'mode':'custom','max_bytes':32*1024**2}]:
            with self.assertRaises(AssertionError):a.apply(self.host,self.profile,mode)
        self.version.write_text('9.0.0')
        with self.assertRaisesRegex(AssertionError,'allowance'):a.apply(self.host,self.profile, {'mode':'custom','max_bytes':3*GIB})
        self.host.run.assert_not_called();self.assertFalse(self.config.exists())
    def test_foreign_settings_require_explicit_adoption(self):
        (self.config.parent/'foreign.conf').write_text('options zfs zfs_arc_max=123\n')
        with self.assertRaisesRegex(AssertionError,'Conflicting'):a.apply(self.host,self.profile,self.mode)
        self.host.run.assert_not_called();self.assertFalse(self.config.exists())
    def test_failed_initramfs_is_retried_even_after_the_config_file_exists(self):
        self.host.run.side_effect=RuntimeError('injected initramfs failure')
        with self.assertRaisesRegex(RuntimeError,'injected'):a.apply(self.host,self.profile,self.mode)
        self.assertTrue(self.config.exists())
        self.assertEqual((self.params/'zfs_arc_max').read_text(),str(2*GIB))
        self.host.run.side_effect=None
        before=a.observe();after=dict(before,maximum_bytes=GIB)
        with patch.object(a,'observe',side_effect=[before,before,after]):a.apply(self.host,self.profile,self.mode)
        self.assertEqual(self.host.run.call_count,2)
    def test_effective_limit_drift_is_detected(self):
        self.config.write_text(a.desired_text(self.mode,self.profile))
        self.assertFalse(a.enforced(self.mode,self.profile))

    def test_release_waits_for_actual_reclamation_and_restores_temporary_target(self):
        knob=self.params/'zfs_arc_sys_free';knob.write_text('1234')
        mem=self.root/'meminfo';mem.write_text('MemFree: 524288 kB\n')
        record={};saved=[];before=a.observe();after=dict(before,size_bytes=2*GIB)
        with patch.object(a,'MEMINFO',mem),patch.object(a,'observe',side_effect=[before,before,after]):
            a.reclaim_for_release(self.profile,record,lambda v:saved.append(dict(v)))
        self.assertEqual(knob.read_text(),'1234');self.assertEqual(record,{})
        self.assertEqual(saved[0],{'arc_previous_sys_free':1234});self.assertEqual(saved[-1],{})
    def test_reclamation_timeout_never_releases_capacity_and_restores_knob(self):
        knob=self.params/'zfs_arc_sys_free';knob.write_text('5678')
        mem=self.root/'meminfo';mem.write_text('MemFree: 524288 kB\n')
        record={}
        with patch.object(a,'MEMINFO',mem),patch.object(a.time,'monotonic',side_effect=[0,31]):
            with self.assertRaisesRegex(RuntimeError,'pending:.*capacity remains fenced'):
                a.reclaim_for_release(self.profile,record,lambda v:None)
        self.assertEqual(knob.read_text(),'5678');self.assertEqual(record,{})
    def test_interrupted_reclamation_restores_journaled_target_on_recovery(self):
        knob=self.params/'zfs_arc_sys_free';knob.write_text(str(8*GIB))
        record={'arc_previous_sys_free':9876};saved=[]
        a.restore_reclaim(record,lambda v:saved.append(dict(v)))
        self.assertEqual(knob.read_text(),'9876');self.assertEqual(saved,[{}])

    def test_ram_growth_default_minimum_is_accepted_inside_backed_cap(self):
        high=dict(a.observe(),minimum_bytes=3*GIB//2)
        with patch.object(a,'observe',return_value=high):
            a.validate(self.profile,{'mode':'automatic'})
            self.assertNotIn('zfs_arc_min=',a.desired_text({'mode':'automatic'},self.profile))
        self.host.run.assert_not_called()

    def test_coordinated_lowering_keeps_a_durable_floor_and_detects_ignored_write(self):
        high=dict(a.observe(),minimum_bytes=3*GIB//2)
        with patch.object(a,'observe',return_value=high):
            with self.assertRaisesRegex(AssertionError,'coordinated ARC minimum'):
                a.apply(self.host,self.profile,self.mode)
        self.assertEqual((self.params/'zfs_arc_min').read_text(),str(GIB//2))
        self.assertIn('zfs_arc_min='+str(GIB//2),self.config.read_text())
        # The max was never written after the kernel ignored the min.
        self.assertEqual((self.params/'zfs_arc_max').read_text(),str(2*GIB))
