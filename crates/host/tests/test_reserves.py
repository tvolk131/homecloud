"""Reserve fault tests: disposable files and mocked ZFS/systemd, without root."""
import copy
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

spec=importlib.util.spec_from_file_location('reserve_host_runtime',Path(__file__).parents[1]/'host_runtime.py')
host=importlib.util.module_from_spec(spec)
sys.modules[spec.name]=host
spec.loader.exec_module(host)
r=host.resource_reserves
GIB=1024**3
VM='00000000-0000-4000-8000-000000000001'
OP='00000000-0000-4000-8000-000000000002'

class ReserveTests(unittest.TestCase):
    def setUp(self):
        tmp=tempfile.TemporaryDirectory();self.addCleanup(tmp.cleanup);self.path=Path(tmp.name)
        self.base={'capacity':{'memory_bytes':8*GIB,'cpu_cores':2,'disk_bytes':1000*GIB},
                   'cpu_groups':[[2,8],[3,9]],'housekeeping':[0,6],'minimum_pool_available_bytes':30*GIB,
                   'pool':'tank','dataset_root':'tank/homecloud','disk_overhead_gib_per_vm':2}
        self.op={'id':OP,'phase':'applying','before':r.initial()['state'],
                 'target':{'revision':1,'additional':{'memory_bytes':GIB,'cpu_cores':1,'disk_bytes':50*GIB},'core_groups':[[3,9]]}}
        self.users=[{'grant':{'memory_bytes':4*GIB,'cpu_cores':1,'disk_bytes':900*GIB},
                     'committed':{'memory_bytes':GIB,'cpu_cores':1,'disk_bytes':12*GIB}}]
        self.vms=[{'id':VM,'cpu_ids':[2,8],'observed':'running'}]
        def save(path,value):path.write_text(json.dumps(value));path.chmod(0o600)
        self.host=SimpleNamespace(STATE=self.path,save=save,backing=Mock(),zget=Mock(return_value=999*GIB))
    def record(self):
        p=self.path/'resource-reserves.json'
        return json.loads(p.read_text()) if p.exists() else r.initial()
    def config(self):return r.effective(self.base,self.record())
    def apply(self):return r.apply(self.host,self.config(),self.op,self.vms,self.users)

    def test_unused_grants_and_claimed_cores_are_protected(self):
        self.op['target']['additional']['disk_bytes']=101*GIB
        with patch.object(r,'enforce') as enforce:
            with self.assertRaisesRegex(AssertionError,'promised'):self.apply()
            enforce.assert_not_called()
        self.op['target']['additional']['disk_bytes']=50*GIB
        self.vms[0]['cpu_ids']=[3,9]
        with self.assertRaisesRegex(AssertionError,'claimed physical core'):self.apply()
        self.assertFalse((self.path/'resource-reserves.json').exists())
        self.host.backing.assert_not_called()

    def test_old_host_claims_and_split_smt_fail_before_effects(self):
        (self.path/'cpu-vms.json').write_text(json.dumps([{'cpu_ids':[3,9]}]))
        with self.assertRaisesRegex(AssertionError,'still holds'):self.apply()
        self.op['target']['core_groups']=[[3]]
        with self.assertRaisesRegex(AssertionError,'physical cores'):self.apply()
        self.host.backing.assert_not_called()

    def test_deleted_history_does_not_own_cores_but_unconfirmed_stopped_claims_do(self):
        deleted={'cpu_ids':[3,9],'observed':'deleted'}
        self.vms.append(deleted)
        (self.path/'cpu-vms.json').write_text(json.dumps([deleted]))
        with patch.object(r,'enforce') as enforce:
            self.apply();enforce.assert_called_once()
        (self.path/'resource-reserves.json').unlink()
        self.vms[-1]['observed']='stopped'
        with self.assertRaisesRegex(AssertionError,'claimed physical core'):self.apply()

    def test_insufficient_physical_pool_floor_keeps_old_policy(self):
        self.host.zget.return_value=79*GIB
        with patch.object(r,'enforce') as enforce:
            with self.assertRaisesRegex(AssertionError,'physical pool headroom'):self.apply()
            enforce.assert_not_called()
        self.assertEqual(self.record()['state'],r.initial()['state'])

    def test_failure_is_unconfirmed_and_replays_after_restart(self):
        with patch.object(r,'enforce',side_effect=RuntimeError('injected failure')):
            with self.assertRaisesRegex(RuntimeError,'injected'):self.apply()
        self.assertEqual(self.record()['phase'],'applying')
        self.assertEqual(self.record()['state'],self.op['target'])
        with patch.object(r,'enforce') as enforce:
            result=self.apply();enforce.assert_called_once()
        self.assertFalse(result['applying'])
        self.assertEqual(self.record()['phase'],'applied')

    def test_boot_replays_journal_before_confirming_it(self):
        with patch.object(r,'enforce',side_effect=RuntimeError('interrupted')):
            with self.assertRaises(RuntimeError):self.apply()
        with patch.object(r,'enforce') as enforce:
            r.boot(self.host,self.config(),self.vms)
            self.assertEqual(enforce.call_args.args[1]['housekeeping'],[0,6,3,9])
        self.assertEqual(self.record()['phase'],'applied')

    def test_late_duplicate_uses_current_host_vm_state(self):
        with patch.object(r,'enforce'):self.apply()
        (self.path/'cpu-vms.json').write_text(json.dumps(self.vms))
        self.host.backing.reset_mock()
        with patch.object(r,'enforce') as enforce,patch.object(r,'verify') as verify:
            r.apply(self.host,self.config(),self.op,[{'cpu_ids':[3,9]}],self.users)
            enforce.assert_not_called();self.host.backing.assert_not_called()
            verify.assert_called_once_with(self.host,self.config(),self.vms)

    def test_compute_only_change_does_not_rebuild_initramfs(self):
        with patch.object(r,'enforce'),patch.object(r.arc_policy,'apply') as apply:
            self.apply()
        self.assertFalse(self.record()['arc_persistence_required'])
        self.assertFalse(apply.call_args.kwargs['persist'])

    def test_boot_finishes_interrupted_cache_persistence_before_confirming_it(self):
        self.op['target']['arc']={'mode':'automatic'}
        with patch.object(r,'enforce'),patch.object(r.arc_policy,'validate'),patch.object(r.arc_policy,'apply',side_effect=RuntimeError('interrupted initramfs')):
            with self.assertRaisesRegex(RuntimeError,'interrupted initramfs'):self.apply()
        self.assertEqual(self.record()['phase'],'applying')
        self.assertTrue(self.record()['arc_persistence_required'])
        with patch.object(r,'enforce'),patch.object(r.arc_policy,'apply',side_effect=RuntimeError('still interrupted')):
            r.boot(self.host,self.config(),self.vms)
        self.assertEqual(self.record()['cache_error'],'still interrupted')
        self.assertEqual(self.record()['phase'],'applying')
        with patch.object(r,'enforce'),patch.object(r.arc_policy,'apply') as apply:
            r.boot(self.host,self.config(),self.vms)
            self.assertTrue(apply.call_args.kwargs['persist'])
        self.assertEqual(self.record()['phase'],'applied')
        with patch.object(r,'enforce'),patch.object(r.arc_policy,'apply') as apply:
            r.boot(self.host,self.config(),self.vms)
            self.assertFalse(apply.call_args.kwargs['persist'])

    def test_arc_persistence_requirement_survives_interrupted_operation(self):
        self.op['target']['arc']={'mode':'automatic'}
        with patch.object(r.arc_policy,'validate'),patch.object(r.arc_policy,'enforced',return_value=True),patch.object(r,'enforce'),patch.object(r.arc_policy,'apply',side_effect=RuntimeError('interrupted')):
            with self.assertRaises(RuntimeError):self.apply()
        self.assertTrue(self.record()['arc_persistence_required'])
        with patch.object(r.arc_policy,'validate'),patch.object(r.arc_policy,'enforced',return_value=True),patch.object(r,'enforce'),patch.object(r.arc_policy,'apply') as apply:
            self.apply()
        self.assertTrue(apply.call_args.kwargs['persist'])

    def test_discard_tombstone_blocks_late_apply(self):
        result=r.discard(self.host,self.config(),self.op)
        self.assertEqual(result['operation_id'],OP)
        self.assertIn(OP,self.record()['discarded'])
        with patch.object(r,'enforce') as enforce:
            with self.assertRaisesRegex(AssertionError,'discarded'):self.apply()
            enforce.assert_not_called()
        self.assertEqual(self.record()['state'],r.initial()['state'])
        self.op['id']='00000000-0000-4000-8000-000000000003'
        with patch.object(r,'enforce'):self.apply()
        with self.assertRaisesRegex(AssertionError,'effects have begun'):
            r.discard(self.host,self.config(),self.op)

    def test_runtime_reloads_config_after_acquiring_operation_lock(self):
        first=self.config();second=copy.deepcopy(first);second['marker']='after-lock'
        request={'kind':'apply_reserves','operation':self.op,'vms':self.vms,'users':self.users}
        with patch.object(host,'STATE',self.path),patch.object(host,'load_config',side_effect=[first,second]),patch.object(host.os,'geteuid',return_value=0),patch.object(host.sys,'argv',['host_runtime.py']),patch.object(host.sys,'stdin',io.StringIO(json.dumps(request))),patch.object(r,'apply',return_value={}) as apply:
            host.main();self.assertEqual(apply.call_args.args[1]['marker'],'after-lock')

    def test_status_does_not_wait_for_workload_lock(self):
        with patch.object(host,'STATE',self.path),patch.object(host,'load_config',return_value=self.config()),patch.object(host.os,'geteuid',return_value=0),patch.object(host.sys,'argv',['host_runtime.py']),patch.object(host.sys,'stdin',io.StringIO('{"kind":"reserve_status"}')):
            result=host.main()
        self.assertEqual(result['kind'],'reserve_policy')
        self.assertFalse((self.path/'operations.lock').exists())

    def test_live_and_persistent_cpu_fences_change_without_guest_stop(self):
        real_path=Path
        def mapped(path):return self.path/str(path).lstrip('/')
        config=r.effective(self.base,{'state':self.op['target'],'phase':'applying','operation_id':OP})
        for name,cpus in [('system.slice','0,3,6,9'),('user.slice','0,3,6,9'),('homecloud.slice/homecloud-'+VM.replace('-','')+'.service','0,2,3,6,8,9')]:
            p=mapped('/sys/fs/cgroup/'+name+'/cpuset.cpus.effective');p.parent.mkdir(parents=True,exist_ok=True);p.write_text(cpus)
        def text(path,value):
            real_path(path).parent.mkdir(parents=True,exist_ok=True);real_path(path).write_text(value)
        adapter=SimpleNamespace(cpulist=host.cpulist,parse_cpus=host.parse_cpus,text=text,run=Mock(),cpu_layout=Mock(),cpu_policy=Mock())
        with patch.object(r,'Path',side_effect=mapped),patch.object(r.os,'sched_getaffinity',return_value={0,3,6,9},create=True):r.enforce(adapter,config,self.vms)
        self.assertIn('AllowedCPUs=0,3,6,9',mapped('/etc/systemd/system/system.slice.d/zz-homecloud-reserves.conf').read_text())
        calls=[call.args for call in adapter.run.call_args_list]
        self.assertIn(('systemctl','set-property','--runtime','system.slice','AllowedCPUs=0,3,6,9'),calls)
        self.assertTrue(all('stop' not in call and 'restart' not in call for call in calls))
        adapter.cpu_policy.assert_called_once_with(config,self.vms,activate=False)

    def test_release_stays_applying_until_cache_reclamation_is_confirmed(self):
        before=copy.deepcopy(self.op['target']);before['additional']['memory_bytes']=2*GIB
        before['revision']=1
        record={'state':before,'operation_id':OP,'phase':'applied','discarded':[]}
        r.write_record(self.host,record)
        self.op={'id':'00000000-0000-4000-8000-000000000003','phase':'applying','before':before,
                 'target':copy.deepcopy(before)}
        self.op['target']['revision']=2;self.op['target']['additional']['memory_bytes']=0
        with patch.object(r,'enforce'),patch.object(r.arc_policy,'reclaim_for_release',side_effect=RuntimeError('pending: reclamation')):
            with self.assertRaisesRegex(RuntimeError,'pending:'):self.apply()
        self.assertEqual(self.record()['phase'],'applying')
        self.assertFalse(self.record()['arc_persistence_required'])
        with patch.object(r,'enforce'),patch.object(r.arc_policy,'reclaim_for_release') as reclaim:
            self.assertFalse(self.apply()['applying']);reclaim.assert_called_once()
        self.assertEqual(self.record()['phase'],'applied')

    def test_first_arc_change_accepts_a_rust_serialized_unset_before_state(self):
        # This literal is the Rust HostReserves::default wire representation.
        # Python's old missing-key initial state rejected this before effects.
        self.op['before']={'revision':0,'additional':dict.fromkeys(r.KEYS,0),'core_groups':[],'arc':None}
        self.op['target']['arc']={'mode':'automatic'}
        with patch.object(r,'enforce'),patch.object(r.arc_policy,'validate'),patch.object(r.arc_policy,'apply'),patch.object(r.arc_policy,'enforced',return_value=True):
            result=self.apply()
        self.assertFalse(result['applying'])
        self.assertEqual(self.record()['phase'],'applied')
        self.assertEqual(self.record()['state']['arc'],{'mode':'automatic'})

    def test_arc_drift_is_distinct_from_an_unfinished_reserve_apply(self):
        record=r.initial()
        with patch.object(r.memory_capacity,'status',return_value={'arc_ready':False}),patch.object(r.arc_policy,'observe',return_value=None),patch.object(r.arc_policy,'policy',return_value=None),patch.object(r.arc_policy,'enforced',return_value=False):
            report=r.report(self.base,record)
            self.assertFalse(report['applying'])
            self.assertFalse(report['memory']['arc_ready'])
            record['phase']='applying'
            self.assertTrue(r.report(self.base,record)['applying'])

    def test_unmanaged_arc_drift_requires_a_concrete_mode_before_any_effects(self):
        with patch.object(r.arc_policy,'enforced',return_value=False),patch.object(r,'enforce') as enforce:
            with self.assertRaisesRegex(AssertionError,'Choose Automatic or a custom ARC limit'):self.apply()
            enforce.assert_not_called()
        self.assertFalse((self.path/'resource-reserves.json').exists())
        self.host.backing.assert_not_called()
