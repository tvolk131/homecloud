"""Protocol and fault tests; no root, Incus, ZFS or physical devices are touched."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('host_runtime', Path(__file__).parents[1] / 'host_runtime.py')
host = importlib.util.module_from_spec(spec)
sys.modules[spec.name]=host
spec.loader.exec_module(host)
_fixture_state=tempfile.TemporaryDirectory()
host.STATE=Path(_fixture_state.name)
def tearDownModule():_fixture_state.cleanup()
VM_ID = '00000000-0000-4000-8000-000000000001'
GPU = '0000:01:00.0'


class LifecycleTests(unittest.TestCase):
    def test_t3_cloud_init_installs_its_runtime_dependency_before_setup(self):
        key = 'ssh-ed25519 fixture-public-key'
        plain = json.loads(host.cloud_init(key).split('\n', 1)[1])
        self.assertNotIn('libatomic1', plain['packages'])
        with patch.object(Path, 'read_text', return_value='# fixture setup script'):
            with_t3 = json.loads(host.cloud_init(key, '0.1.2').split('\n', 1)[1])
        self.assertIn('libatomic1', with_t3['packages'])
        apt=next(f['content'] for f in plain['write_files'] if f['path'].endswith('80-homecloud-retries'))
        self.assertIn('Acquire::Retries "3";',apt)
        self.assertIn('Acquire::ForceIPv4 "true";',apt)
        self.assertTrue(with_t3['package_update'])
        self.assertNotIn('setup-t3.sh', json.dumps(with_t3['runcmd']))

    def test_new_instance_payload_never_combines_agent_templates_with_a_seed_disk(self):
        self.vm.update(desired='running',gpu=None,gpus=[])
        self.vm['spec']['ssh_public_key']='ssh-ed25519 fixture-public-key'
        for transport in ('agent','disk'):
            payloads=[]
            def api(method,path,data=None,**kwargs):
                if method=='GET':return None
                if path=='/1.0/instances':payloads.append(data)
                return {}
            instance={'config':{'boot.autostart':'true','limits.cpu':'4,10','limits.memory':'4GiB'},
                      'devices':{'root':{'size':'10GiB'}}}
            with patch.multiple(host,cpu_policy=Mock(),backing=Mock(),api=api,
                owned=Mock(side_effect=[None,instance]),gpu_preflight=Mock(),
                start=Mock(side_effect=RuntimeError('launch boundary'))):
                with self.assertRaisesRegex(RuntimeError,'launch boundary'):
                    host.apply({**self.config,'image':'a'*64,'cloud_init_transport':transport},
                               self.vm,[self.vm],[],{'kind':'create'})
            self.assertEqual(len(payloads),1)
            self.assertIn('cloud-init.user-data',payloads[0]['config'])
            self.assertEqual('cloud-init' in payloads[0]['devices'],transport=='disk')
        with self.assertRaisesRegex(AssertionError,'transport'):host.seed_devices({'cloud_init_transport':'both'})

    def setUp(self):
        self.vm = {
            'id': VM_ID, 'owner': '00000000-0000-4000-8000-000000000002',
            'spec': {'memory_gib': 4, 'cores': 1, 'disk_gib': 10},
            'resources': {'memory_bytes': 4 * host.GIB, 'cpu_cores': 1, 'disk_bytes': 12 * host.GIB},
            'cpu_ids': [4, 10], 'network_slot': 1, 'gpu': GPU,
            'preferred_gpu': GPU, 'desired': 'stopped', 'observed': 'running', 'ssh_host_key': 'pinned',
        }
        self.instance = {'config': {'boot.autostart': 'true'}, 'devices': {
            'root': {'size': '10GiB'}, 'gpu0': {'type': 'gpu', 'pci': GPU},
        }}
        self.config = {'capacity': {'memory_bytes': 8 * host.GIB}}

    def stop_apply(self, failing=None, force=False):
        events = []
        def event(name):
            def call(*args, **kwargs):
                events.append(name)
                if name == 'stop':
                    self.assertEqual(kwargs.get('force', False), force)
                if failing == name:
                    raise RuntimeError('injected ' + name + ' failure')
                return {}
            return call
        def api(method, path, data=None, **kwargs):
            self.assertEqual(method, 'PUT')
            self.assertEqual(data['config']['boot.autostart'], 'false')
            self.assertNotIn('gpu0', data['devices'])
            events.append('disable-autostart-and-detach')
            return {}
        with patch.multiple(host, cpu_policy=Mock(), backing=Mock(), owned=Mock(return_value=copy.deepcopy(self.instance)),
                            stop=event('stop'), api=api, restore_gpu=event('gpu-cleanup'), release_cpu_policy=event('cpu-cleanup')):
            if failing:
                with self.assertRaisesRegex(RuntimeError, 'injected'):
                    host.apply(self.config, self.vm, [self.vm], [], {'kind': 'force_stop'} if force else {'kind': 'state', 'desired': 'stopped'})
            else:
                result = host.apply(self.config, self.vm, [self.vm], [], {'kind': 'force_stop'} if force else {'kind': 'state', 'desired': 'stopped'})
                self.assertEqual(result, {'state': 'stopped', 'ssh_host_key': 'pinned', 'compute_released': True})
        return events

    def test_stop_confirms_only_after_all_cleanup(self):
        self.assertEqual(self.stop_apply(), ['stop', 'disable-autostart-and-detach', 'gpu-cleanup', 'cpu-cleanup'])

    def test_failed_shutdown_does_not_detach_or_release(self):
        self.assertEqual(self.stop_apply('stop'), ['stop'])

    def test_failed_gpu_cleanup_cannot_confirm_release(self):
        self.assertEqual(self.stop_apply('gpu-cleanup'), ['stop', 'disable-autostart-and-detach', 'gpu-cleanup'])

    def test_force_stop_uses_the_same_verified_cleanup_boundary(self):
        self.assertEqual(self.stop_apply(force=True), ['stop', 'disable-autostart-and-detach', 'gpu-cleanup', 'cpu-cleanup'])
        self.assertEqual(self.stop_apply('stop', force=True), ['stop'])
        self.assertEqual(self.stop_apply('gpu-cleanup', force=True), ['stop', 'disable-autostart-and-detach', 'gpu-cleanup'])

    def test_force_is_explicit_and_command_success_still_requires_observation(self):
        with patch.object(host, 'state', side_effect=['running', 'running']), patch.object(host, 'run', return_value=subprocess.CompletedProcess([], 0)) as run:
            with self.assertRaisesRegex(RuntimeError, 'force stop did not complete'):
                host.stop(self.vm, force=True)
            run.assert_called_once_with('incus', 'stop', host.vm_name(self.vm), '--force', check=False, timeout=60)

    def test_graceful_timeout_never_automatically_forces(self):
        with patch.object(host, 'state', side_effect=['running', 'running']), patch.object(host, 'run', side_effect=subprocess.TimeoutExpired('incus', 60)) as run:
            with self.assertRaisesRegex(RuntimeError, 'graceful shutdown did not complete'):
                host.stop(self.vm)
            run.assert_called_once_with('incus', 'stop', host.vm_name(self.vm), '--timeout', '30', check=False, timeout=60)

    def test_force_restart_recovery_does_not_power_off_an_already_stopped_vm_again(self):
        with patch.object(host, 'state', return_value='stopped'), patch.object(host, 'run') as run:
            host.stop(self.vm, force=True)
            run.assert_not_called()

    def test_force_mutation_cannot_be_used_to_change_a_running_intent(self):
        self.vm['desired'] = 'running'
        with patch.object(host, 'cpu_policy') as policy, self.assertRaisesRegex(AssertionError, 'stopped intent'):
            host.apply(self.config, self.vm, [self.vm], [], {'kind': 'force_stop'})
        policy.assert_not_called()

    def test_successful_command_is_not_enough_without_confirmed_stopped_state(self):
        with patch.object(host, 'state', side_effect=['running', 'running']), patch.object(host, 'run', return_value=subprocess.CompletedProcess([], 0)):
            with self.assertRaisesRegex(RuntimeError, 'shutdown did not complete'):
                host.stop(self.vm)

    def test_cpu_cleanup_refuses_to_kill_remaining_vm_processes(self):
        with tempfile.TemporaryDirectory() as temp:
            unit = 'homecloud-' + VM_ID.replace('-', '') + '.service'
            group = Path(temp) / unit
            group.mkdir()
            (group / 'cgroup.procs').write_text('111\n222\n')
            with patch.object(host, 'Path', return_value=Path(temp)), patch.object(host, 'cpu_policy') as policy, patch.object(host, 'run', return_value=subprocess.CompletedProcess([], 0, stdout='111\n')) as run:
                with self.assertRaisesRegex(AssertionError, 'host processes remain'):
                    host.release_cpu_policy(self.config, self.vm, [self.vm])
                policy.assert_not_called()
                self.assertEqual(run.call_count, 1)  # query MainPID; never stop the unit

    def test_gpu_preference_cannot_override_cpu_only_boot_or_internal_resize(self):
        self.vm['desired'] = 'running'
        self.assertIsNone(host.target_gpu(self.vm, {'kind': 'start', 'without_gpu': True}))
        self.assertEqual(host.target_gpu(self.vm, {'kind': 'start', 'without_gpu': False}), GPU)
        self.assertIsNone(host.target_gpu(self.vm, {'kind': 'gpu', 'pci': None}))
        self.vm['gpu'] = None  # completed CPU-only boot still remembers GPU
        self.assertIsNone(host.target_gpu(self.vm, {'kind': 'resize'}))
        self.vm['desired'] = 'stopped'
        self.assertIsNone(host.target_gpu(self.vm, {'kind': 'state'}))

    def test_gpu_restore_refuses_a_device_still_attached_to_any_instance(self):
        with tempfile.TemporaryDirectory() as temp:
            saved = Path(temp) / 'gpu-0000_01_00.0.json'
            saved.write_text(json.dumps({'drivers': {}, 'vtconsoles': [], 'getty': False}))
            with patch.object(host, 'STATE', Path(temp)), patch.object(host, 'api', return_value=[{'expanded_devices': {'gpu': {'type': 'gpu', 'pci': GPU}}}]):
                with self.assertRaisesRegex(AssertionError, 'still attached'):
                    host.restore_gpu(GPU)
            self.assertTrue(saved.exists())  # recovery metadata remains intact

    def test_released_cpu_claims_are_omitted_from_startup_policy(self):
        self.vm['cpu_ids'] = []
        self.vm['resources']['cpu_cores'] = 0
        self.vm['resources']['memory_bytes'] = 0
        config = {'housekeeping': [0], 'legacy_vms': [], 'cpu_groups': [[4, 10]]}
        with patch.object(host, 'text', return_value=False) as text, patch.object(host, 'save'), patch.object(host, 'run') as run:
            host.cpu_policy(config, [self.vm])
            self.assertEqual(text.call_args.args[1], '')
            run.assert_not_called()
            self.vm['desired'] = 'running'
            with self.assertRaisesRegex(AssertionError, 'no CPU claim'):
                host.cpu_policy(config, [self.vm])

    def test_delete_fully_stopped_vm_does_not_require_reacquiring_compute(self):
        self.vm.update(desired='deleted', observed='stopped', cpu_ids=[], gpu=None)
        self.vm['resources'].update(memory_bytes=0, cpu_cores=0)
        self.instance['devices'].pop('gpu0')
        config = {**self.config, 'housekeeping': [0], 'legacy_vms': [], 'cpu_groups': [[4, 10]]}
        events = []
        def api(method, path, data=None, **kwargs):
            events.append((method, path))
            if method == 'GET':
                return {'config': {'user.homecloud.id': VM_ID}}
            return {}
        with patch.multiple(host, text=Mock(return_value=False), save=Mock(), run=Mock(),
                            owned=Mock(return_value=self.instance), stop=Mock(), api=api,
                            restore_released_gpus=Mock(), release_cpu_policy=Mock()):
            result = host.apply(config, self.vm, [self.vm], [], {'kind': 'state', 'desired': 'deleted'})
            self.assertEqual(result, {'state': 'deleted', 'ssh_host_key': None, 'compute_released': True})
            host.release_cpu_policy.assert_called_once()
        self.assertIn(('DELETE', '/1.0/instances/' + host.vm_name(self.vm)), events)
        self.assertIn(('DELETE', '/1.0/networks/hcvm1'), events)

class LaunchReplayTests(unittest.TestCase):
    def test_successful_apply_retains_launch_guard_through_lost_ack_and_guest_reboot(self):
        vm={'id':VM_ID,'owner':'00000000-0000-4000-8000-000000000002',
            'spec':{'memory_gib':4,'cores':1,'disk_gib':10},
            'resources':{'memory_bytes':4*host.GIB,'cpu_cores':1},'cpu_ids':[4,10],
            'network_slot':1,'gpus':[],'pending_gpus':[],'desired':'running',
            'observed':'running','ssh_host_key':'ssh-ed25519 pinned'}
        instance={'config':{'limits.memory':'4GiB','limits.cpu':'4,10','boot.autostart':'true'},
                  'devices':{'root':{'size':'10GiB'}}}
        power={'value':'running'}
        runner=Mock()
        def guest(vm,*args,**kwargs):
            return subprocess.CompletedProcess([],0,json.dumps({'status':'done'}) if args[0]=='cloud-init' else vm['ssh_host_key'],'')
        with tempfile.TemporaryDirectory() as d, patch.object(host,'STATE',Path(d)), \
                patch.multiple(host,cpu_policy=Mock(),backing=Mock(),owned=Mock(return_value=instance),
                    state=lambda vm:power['value'],api=Mock(return_value={'config':{'user.homecloud.id':VM_ID}}),
                    gpu_preflight=Mock(),exec_guest=guest,run=runner), \
                patch.object(host.instance_runtime,'boot_id',return_value='test-boot'), \
                patch.object(host.instance_runtime,'live_processes',return_value=[]):
            marker=host.instance_runtime.launch_path(host,vm)
            marker.write_text(json.dumps({'boot':'test-boot'}))
            result=host.apply({'capacity':{'memory_bytes':8*host.GIB}},vm,[vm],[],{'kind':'resize'})
            self.assertEqual(result['state'],'running')
            self.assertTrue(marker.exists(),'Host completion must not discard the guard before its acknowledgement is durable')
            # Replaying the same host effect after a lost response can coincide
            # with the empty-process window of an Incus-internal guest reboot.
            power['value']='stopped'
            with self.assertRaisesRegex(RuntimeError,'pending:.*previous launch'):
                host.start(vm)
            runner.assert_not_called()
            # Explicitly confirmed shutdown ends this launch epoch and permits
            # a genuinely new, claimed start.
            host.stop(vm)
            self.assertFalse(marker.exists())
            def launch(*args,**kwargs):
                self.assertEqual(args[:2],('incus','start'));power['value']='running'
                return subprocess.CompletedProcess(args,0,stdout='',stderr='')
            with patch.object(host,'api',return_value={}),patch.object(host,'run',side_effect=launch) as run:
                host.start(vm)
            self.assertEqual(run.call_count,1)


class CombinedEditTests(unittest.TestCase):
    def apply_edit(self, fail_stop=False, repeat=False):
        old='0000:01:00.0';new='0000:02:00.0';key='ssh-ed25519 pinned-key'
        vm={'id':VM_ID,'owner':'00000000-0000-4000-8000-000000000002',
            'spec':{'memory_gib':8,'cores':2,'disk_gib':20},'resources':{'memory_bytes':8*host.GIB,'cpu_cores':2},
            'cpu_ids':[4,10,6,12],'network_slot':1,'gpu':old,'pending_gpu':new,
            'desired':'running','observed':'running','ssh_host_key':key}
        state={'power':'running','instance':{'config':{'limits.memory':'4GiB','limits.cpu':'4,10','boot.autostart':'true'},
               'devices':{'root':{'size':'10GiB'},'gpu0':{'type':'gpu','pci':old}}}}
        events=[]
        def api(method,path,data=None,**kwargs):
            if method=='GET' and '/networks/' in path:return {'config':{'user.homecloud.id':VM_ID}}
            if method=='PUT':
                self.assertEqual(state['power'],'stopped')
                events.append('configuration');state['instance']=copy.deepcopy(data);return {}
            raise AssertionError((method,path))
        def stop(*args,**kwargs):
            events.append('stop')
            if fail_stop:raise RuntimeError('conflict: injected graceful shutdown failure')
            state['power']='stopped'
        def gpu_changes(config,current,instance,target):
            self.assertEqual(state['power'],'stopped');self.assertEqual(target,[new])
            events.append('gpu');state['instance']['devices']['gpu0']={'type':'gpu','pci':new}
        def start(*args):
            if state['power']!='running':events.append('boot');state['power']='running'
        def exec_guest(vm,*args,**kwargs):
            return subprocess.CompletedProcess([],0,json.dumps({'status':'done'}) if args[0]=='cloud-init' else key,'')
        def driver(config,current):self.assertEqual(current['gpu'],new);events.append('driver')
        mutation={'kind':'edit','restart_confirmed':True,'changes':{'gpu':{'kind':'set','pci':new}}}
        with patch.multiple(host,cpu_policy=Mock(),backing=Mock(),owned=lambda *a,**k:copy.deepcopy(state['instance']),
                            api=api,stop=stop,start=start,gpu_changes=gpu_changes,gpu_preflight=Mock(),restore_gpu=Mock(),exec_guest=exec_guest,ensure_gpu_driver=driver):
            if fail_stop:
                with self.assertRaisesRegex(RuntimeError,'graceful shutdown failure'):host.apply({'capacity':{'memory_bytes':16*host.GIB},'allowed_gpus':[new]},vm,[vm],[],mutation)
            else:
                result=host.apply({'capacity':{'memory_bytes':16*host.GIB},'allowed_gpus':[new]},vm,[vm],[],mutation)
                self.assertEqual(result['state'],'running');self.assertEqual(result['ssh_host_key'],key)
                if repeat:host.apply({'capacity':{'memory_bytes':16*host.GIB},'allowed_gpus':[new]},vm,[vm],[],mutation)
        return events

    def test_combined_resize_and_gpu_swap_boot_once_and_replay_does_not_restart_again(self):
        events=self.apply_edit(repeat=True)
        self.assertEqual(events.count('boot'),1)
        self.assertLess(events.index('stop'),events.index('configuration'))
        self.assertLess(events.index('gpu'),events.index('boot'))

    def test_failed_graceful_shutdown_applies_no_device_or_resource_changes(self):
        self.assertEqual(self.apply_edit(fail_stop=True),['stop'])

    def test_unconfirmed_edit_is_rejected_before_host_changes(self):
        vm={'id':VM_ID,'owner':'00000000-0000-4000-8000-000000000002',
            'spec':{'memory_gib':4,'cores':1,'disk_gib':10},'network_slot':1,'desired':'running'}
        with patch.object(host,'cpu_policy') as policy,self.assertRaisesRegex(AssertionError,'restart confirmation'):
            host.apply({},vm,[vm],[],{'kind':'edit','restart_confirmed':False})
        policy.assert_not_called()

class MultiGpuTests(unittest.TestCase):
    A='0000:01:00.0'
    B='0000:02:00.0'
    C='0000:03:00.0'

    def vm(self):
        return {'id':VM_ID,'owner':'00000000-0000-4000-8000-000000000002',
            'spec':{'memory_gib':4,'cores':1,'disk_gib':10},'resources':{'memory_bytes':4*host.GIB,'cpu_cores':1},
            'cpu_ids':[4,10],'network_slot':1,'gpus':[self.A,self.B],'pending_gpus':[self.C],
            'preferred_gpus':[self.B,self.C],'desired':'running','observed':'running','ssh_host_key':'ssh-ed25519 pinned'}

    def instance(self):
        return {'config':{'limits.memory':'4GiB','limits.cpu':'4,10','boot.autostart':'true'},
            'devices':{'root':{'type':'disk','size':'10GiB'},'gpu3':{'type':'gpu','pci':self.A},'gpu7':{'type':'gpu','pci':self.B}}}

    def test_replacement_preserves_retained_device_name_and_checks_complete_set(self):
        vm=self.vm();current=self.instance();events=[]
        def api(method,path,data=None,**kwargs):
            self.assertEqual(method,'PUT');events.append('put');current.clear();current.update(copy.deepcopy(data))
        with patch.multiple(host,owned=lambda *a,**k:copy.deepcopy(current),api=api,stop=lambda *a:events.append('stop'),
                            gpu_preflight=Mock(),prepare_gpu=lambda c,p:events.append('prepare:'+p),restore_gpu=lambda p:events.append('restore:'+p)):
            host.gpu_changes({},vm,copy.deepcopy(current),[self.B,self.C])
        self.assertEqual(current['devices']['gpu7']['pci'],self.B)
        self.assertEqual({d['pci'] for d in host.gpu_devices(current).values()},{self.B,self.C})
        self.assertNotIn('gpu3',current['devices'])
        self.assertLess(events.index('stop'),events.index('put'))
        self.assertLess(events.index('restore:'+self.A),events.index('prepare:'+self.C))

    def test_failed_second_card_preparation_never_attaches_a_partial_set_and_retry_is_idempotent(self):
        vm=self.vm();vm.update(gpus=[],pending_gpus=[self.A,self.B]);current=self.instance();current['devices']={'root':{'type':'disk','size':'10GiB'}}
        prepared=[]
        def prepare(config,address):
            prepared.append(address)
            if address==self.B and prepared.count(address)==1:raise RuntimeError('injected second-card failure')
        def api(method,path,data=None,**kwargs):
            self.assertEqual(method,'PUT');current.clear();current.update(copy.deepcopy(data))
        with patch.multiple(host,owned=lambda *a,**k:copy.deepcopy(current),api=api,stop=Mock(),gpu_preflight=Mock(),prepare_gpu=prepare):
            with self.assertRaisesRegex(RuntimeError,'second-card failure'):host.gpu_changes({},vm,copy.deepcopy(current),[self.A,self.B])
            self.assertEqual(host.gpu_devices(current),{})
            self.assertEqual(host.gpu_claims(vm),{self.A,self.B})
            host.gpu_changes({},vm,copy.deepcopy(current),[self.A,self.B])
        self.assertEqual(len(host.gpu_devices(current)),2)
        self.assertEqual({dev['pci'] for dev in host.gpu_devices(current).values()},{self.A,self.B})

    def test_start_claims_do_not_skip_iommu_and_foreign_assignment_preflight(self):
        vm=self.vm();vm.update(gpus=[self.A,self.B],pending_gpus=[])
        foreign={'name':'foreign','expanded_devices':{'display':{'type':'gpu','pci':self.B}}}
        with patch.object(host,'gpu_group') as group,patch.object(host,'api',return_value=[foreign]):
            with self.assertRaisesRegex(AssertionError,'outside this allocation'):
                host.gpu_preflight({'allowed_gpus':[self.A,self.B]},vm,[self.A,self.B],set())
            self.assertEqual({args.args[1] for args in group.call_args_list},{self.A,self.B})

    def test_preflight_failure_on_any_card_happens_before_stop_or_device_changes(self):
        vm=self.vm();vm.update(gpus=[],pending_gpus=[self.A,self.B]);current=self.instance();current['devices'].pop('gpu3');current['devices'].pop('gpu7')
        with patch.object(host,'gpu_group',side_effect=AssertionError('Unsafe IOMMU group')),patch.object(host,'stop') as stop,patch.object(host,'prepare_gpu') as prepare:
            with self.assertRaisesRegex(AssertionError,'Unsafe IOMMU'):
                host.gpu_changes({'allowed_gpus':[self.A,self.B]},vm,current,[self.A,self.B])
            stop.assert_not_called();prepare.assert_not_called()

    def test_unclaimed_inherited_and_duplicate_gpu_configuration_is_never_adopted(self):
        vm=self.vm();current=self.instance()
        current['devices']['foreign']={'type':'gpu','pci':self.C}
        with self.assertRaisesRegex(AssertionError,'unrecognized'):host.checked_gpu_devices(vm,current)
        current=self.instance();current['expanded_devices']={**current['devices'],'inherited':{'type':'gpu','pci':self.C}}
        with self.assertRaisesRegex(AssertionError,'inherited'):host.checked_gpu_devices(vm,current)
        current=self.instance();current['devices']['gpu8']={'type':'gpu','pci':self.B}
        with self.assertRaisesRegex(AssertionError,'duplicate'):host.checked_gpu_devices(vm,current)
        current=self.instance();current['devices']['gpu7']['pci']='0000:09:00.0'
        with self.assertRaisesRegex(AssertionError,'assignment drift'):host.checked_gpu_devices(vm,current)

    def test_observation_compares_all_gpus_instead_of_only_gpu0(self):
        vm=self.vm();vm['pending_gpus']=[]
        current=self.instance();current.update(name=host.vm_name(vm),status='Running');current['config'].update({'user.homecloud.id':vm['id'],'user.homecloud.owner':vm['owner']})
        with patch.object(host,'api',return_value=[current]):
            self.assertIsNone(host.observe([vm])[0]['error'])
            current['devices']['gpu7']['pci']=self.C
            self.assertIn('Configuration drift',host.observe([vm])[0]['error'])

    def test_all_gpu_cleanup_finishes_before_cpu_release_and_failure_keeps_claims(self):
        vm=self.vm();vm['desired']='stopped';events=[]
        def api(method,path,data=None,**kwargs):
            self.assertEqual(host.gpu_devices(data),{});events.append('detach-all')
        def restore(address):
            events.append(address)
            if address==self.C:raise RuntimeError('injected third-card reset failure')
        with patch.multiple(host,cpu_policy=Mock(),backing=Mock(),owned=Mock(return_value=self.instance()),stop=Mock(),api=api,restore_gpu=restore,release_cpu_policy=Mock()):
            with self.assertRaisesRegex(RuntimeError,'third-card'):host.apply({'capacity':{'memory_bytes':8*host.GIB}},vm,[vm],[],{'kind':'state','desired':'stopped'})
            host.release_cpu_policy.assert_not_called()
        self.assertEqual(events,['detach-all',self.A,self.B,self.C])
        self.assertEqual(host.gpu_claims(vm),{self.A,self.B,self.C})

    def test_full_multi_gpu_edit_replay_does_not_repeat_a_successful_boot(self):
        vm=self.vm();vm.update(gpus=[self.A],pending_gpus=[self.B],preferred_gpus=[self.A,self.B])
        current=self.instance();current['devices'].pop('gpu7');power=['running'];events=[]
        def stop(*args,**kwargs):
            if power[0]!='stopped':events.append('stop');power[0]='stopped'
        def start(*args):
            if power[0]!='running':events.append('boot');power[0]='running'
        def api(method,path,data=None,**kwargs):
            if method=='GET' and '/networks/' in path:return {'config':{'user.homecloud.id':VM_ID}}
            self.assertEqual(method,'PUT');self.assertEqual(power[0],'stopped');current.clear();current.update(copy.deepcopy(data))
        def guest(vm,*args,**kwargs):
            stdout=json.dumps({'status':'done'}) if args[0]=='cloud-init' else ('GPU-a\nGPU-b\n' if args[0]=='nvidia-smi' else vm['ssh_host_key'])
            return subprocess.CompletedProcess([],0,stdout,'')
        mutation={'kind':'edit','restart_confirmed':True,'changes':{'gpu':{'kind':'set_many','pcis':[self.A,self.B]}}}
        with patch.multiple(host,cpu_policy=Mock(),backing=Mock(),owned=lambda *a,**k:copy.deepcopy(current),gpu_preflight=Mock(),prepare_gpu=Mock(),restore_gpu=Mock(),stop=stop,start=start,api=api,exec_guest=guest,ensure_gpu_driver=Mock()):
            for _ in range(2):
                self.assertEqual(host.apply({'capacity':{'memory_bytes':8*host.GIB}},vm,[vm],[],mutation)['state'],'running')
        self.assertEqual(events,['stop','boot'])
        self.assertEqual(len(host.gpu_devices(current)),2)

    def test_incus_reads_have_a_short_bound_so_inventory_cannot_hold_management_open(self):
        with patch.object(host,'run',return_value=subprocess.CompletedProcess([],0,stdout='{}',stderr='')) as run:
            host.api('GET','/1.0/instances?recursion=1')
            self.assertEqual(run.call_args.kwargs['timeout'],8)
            host.api('POST','/1.0/instances',{'name':'owned'})
            self.assertEqual(run.call_args.kwargs['timeout'],90)

    def test_unavailable_incus_does_not_represent_cached_running_state_as_fresh(self):
        vm={'id':VM_ID,'observed':'running','resources':{'memory_bytes':6*host.GIB}};before=copy.deepcopy(vm)
        with patch.object(host,'api',side_effect=subprocess.TimeoutExpired('incus',8)):
            result=host.observe([vm])
        self.assertEqual(result[0]['state'],'unavailable')
        self.assertIn('allocations remain held',result[0]['error'])
        self.assertEqual(vm,before)
