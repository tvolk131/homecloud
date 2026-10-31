"""Host power lifecycle fault tests. All OS/Incus commands are mocked."""
import copy
import fcntl
import io
import json
import select
import subprocess
import textwrap
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

spec=importlib.util.spec_from_file_location('shutdown_host_runtime',Path(__file__).parents[1]/'host_runtime.py')
host=importlib.util.module_from_spec(spec)
sys.modules[spec.name]=host
spec.loader.exec_module(host)
BOOT='00000000-0000-4000-8000-000000000010'
OP='00000000-0000-4000-8000-000000000020'
VM={'id':'00000000-0000-4000-8000-000000000001','desired':'running','gpus':['0000:01:00.0'],'cpu_ids':[4,10]}

class ShutdownTests(unittest.TestCase):
    def setUp(self):
        self.directory=tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.state=patch.object(host,'STATE',Path(self.directory.name))
        self.state.start();self.addCleanup(self.state.stop)

    def test_cancel_tombstone_rejects_delayed_stop_and_poweroff_after_helper_restart(self):
        with patch.object(host,'shutdown_boot',return_value=BOOT),patch.object(host,'stop') as stop,patch.object(host,'run') as run:
            host.cancel_shutdown(OP,BOOT)
            host.cancel_shutdown(OP,BOOT)  # A lost reply is safe to replay.
            self.assertTrue(host.shutdown_fence_path(OP).exists())
            with self.assertRaisesRegex(AssertionError,'cancellation'):host.drain_guest(OP,VM,BOOT)
            self.assertIn('cancellation',host.power_off(OP,BOOT,[VM])['data']['message'])
            stop.assert_not_called();run.assert_not_called()
            # A different operation can still proceed on this boot.
            self.assertEqual(host.check_shutdown_fence('00000000-0000-4000-8000-000000000021',BOOT),{'kind':'ok'})

    def test_drain_stops_gracefully_without_releasing_or_changing_devices(self):
        vm=copy.deepcopy(VM)
        with patch.multiple(host,shutdown_boot=Mock(return_value=BOOT),owned=Mock(),stop=Mock(),restore_gpu=Mock(),release_cpu_policy=Mock(),api=Mock()):
            self.assertEqual(host.drain_guest(OP,vm,BOOT),{'kind':'ok'})
            host.stop.assert_called_once_with(vm)
            host.restore_gpu.assert_not_called();host.release_cpu_policy.assert_not_called();host.api.assert_not_called()
        self.assertEqual(vm,VM)

    def test_old_boot_cannot_stop_or_power_off_anything(self):
        with patch.object(host,'shutdown_boot',return_value='different'),patch.object(host,'run') as run,patch.object(host,'stop') as stop:
            with self.assertRaisesRegex(AssertionError,'restarted'):host.drain_guest(OP,VM,BOOT)
            self.assertIn('restarted',host.power_off(OP,BOOT,[VM])['data']['message'])
            run.assert_not_called();stop.assert_not_called()

    def test_shutdown_failure_is_propagated_without_a_force_fallback(self):
        with patch.multiple(host,shutdown_boot=Mock(return_value=BOOT),owned=Mock(),stop=Mock(side_effect=RuntimeError('guest stuck')),run=Mock()):
            with self.assertRaisesRegex(RuntimeError,'guest stuck'):host.drain_guest(OP,VM,BOOT)
            host.stop.assert_called_once_with(VM);host.run.assert_not_called()

    def test_any_active_instance_including_unmanaged_prevents_poweroff(self):
        for state in ['Running','Frozen','Stopping','Error']:
            with patch.multiple(host,shutdown_boot=Mock(return_value=BOOT),api=Mock(return_value=[{'name':'unmanaged','status':state}]),run=Mock()):
                self.assertIn('still active',host.power_off(OP,BOOT,[VM])['data']['message'])
                host.api.assert_called_once_with('GET','/1.0/instances?recursion=1&all-projects=true')
                host.run.assert_not_called()

    def test_stopped_incus_state_with_live_qemu_prevents_poweroff(self):
        with patch.multiple(host,shutdown_boot=Mock(return_value=BOOT),api=Mock(return_value=[{'status':'Stopped'}]),owned=Mock(),state=Mock(return_value='stopped'),run=Mock()),patch.object(host.instance_runtime,'live_processes',return_value=True):
            self.assertIn('not confirmed',host.power_off(OP,BOOT,[VM])['data']['message'])
            host.run.assert_not_called()

    def test_poweroff_only_queues_normal_os_shutdown_after_all_checks(self):
        with patch.multiple(host,shutdown_boot=Mock(return_value=BOOT),api=Mock(return_value=[{'status':'Stopped'}]),owned=Mock(),state=Mock(return_value='stopped'),run=Mock()),patch.object(host.instance_runtime,'live_processes',return_value=False):
            self.assertEqual(host.power_off(OP,BOOT,[VM]),{'kind':'power_off_queued','data':{'operation_id':OP,'boot_id':BOOT}})
            host.run.assert_called_once_with('systemctl','poweroff',timeout=15)

    def test_pre_poweroff_refusal_is_a_matching_structured_response(self):
        with patch.object(host,'shutdown_boot',return_value=BOOT),patch.object(host,'api',return_value=[{'project':'other','status':'Running'}]),patch.object(host,'run') as run:
            reply=host.power_off(OP,BOOT,[VM])
            self.assertEqual(reply['kind'],'power_off_rejected')
            self.assertEqual(reply['data']['operation_id'],OP)
            self.assertEqual(reply['data']['boot_id'],BOOT)
            run.assert_not_called()

    def test_errors_after_the_os_command_starts_never_claim_a_safe_refusal(self):
        with patch.object(host,'shutdown_boot',return_value=BOOT),patch.object(host,'check_shutdown'),patch.object(host,'run',side_effect=RuntimeError('OS reply lost')):
            with self.assertRaisesRegex(RuntimeError,'OS reply lost'):host.power_off(OP,BOOT,[VM])

    def test_shutdown_dispatch_holds_the_operations_lock_for_every_mutating_request(self):
        routes=[
            ('cancel_shutdown',{'operation_id':OP,'boot_id':BOOT}),
            ('check_shutdown_fence',{'operation_id':OP,'boot_id':BOOT}),
            ('check_shutdown',{'boot_id':BOOT,'vms':[VM]}),
            ('drain_guest',{'operation_id':OP,'vm':VM,'boot_id':BOOT}),
            ('power_off',{'operation_id':OP,'boot_id':BOOT,'vms':[VM]}),
        ]
        def locked(*args):
            with (host.STATE/'operations.lock').open('a') as probe:
                with self.assertRaises(BlockingIOError):fcntl.flock(probe,fcntl.LOCK_EX|fcntl.LOCK_NB)
            return {'kind':'ok'}
        for kind,data in routes:
            with self.subTest(kind=kind),patch.object(host.os,'geteuid',return_value=0),patch.object(host,'load_config',return_value={}),patch.object(host.sys,'argv',['host_runtime.py']),patch.object(host.sys,'stdin',io.StringIO(json.dumps({'kind':kind,**data}))),patch.object(host,kind,side_effect=locked) as route:
                self.assertEqual(host.main(),{'kind':'ok'})
                route.assert_called_once_with(*data.values())

    def test_missing_instance_has_actionable_recovery_and_is_not_treated_as_stopped(self):
        with patch.multiple(host,shutdown_boot=Mock(return_value=BOOT),owned=Mock(return_value=None),stop=Mock()):
            with self.assertRaisesRegex(AssertionError,'Cancel host shutdown and recover or delete'):host.drain_guest(OP,VM,BOOT)
            host.owned.assert_called_once_with(VM,missing=True)
            host.stop.assert_not_called()

    def test_graceful_drain_error_only_offers_actions_available_during_host_shutdown(self):
        with patch.multiple(host,shutdown_boot=Mock(return_value=BOOT),owned=Mock(),stop=Mock(side_effect=RuntimeError('conflict: graceful shutdown did not complete; keep waiting or explicitly force stop'))):
            with self.assertRaises(RuntimeError) as caught:host.drain_guest(OP,VM,BOOT)
            self.assertNotIn('force stop',str(caught.exception))
            self.assertIn('Retry host shutdown',str(caught.exception))
            self.assertIn('cancel it',str(caught.exception))

    def test_real_delayed_helper_processes_reject_a_cancellation_written_under_the_lock(self):
        script=textwrap.dedent(r"""
            import importlib.util,io,json,sys
            from pathlib import Path
            spec=importlib.util.spec_from_file_location('delayed_shutdown_host',sys.argv[1])
            h=importlib.util.module_from_spec(spec);sys.modules[spec.name]=h;spec.loader.exec_module(h)
            h.STATE=Path(sys.argv[2]);payload=sys.argv[3];boot=sys.argv[4]
            h.os.geteuid=lambda:0;h.shutdown_boot=lambda:boot
            count=0
            def config():
                global count
                count+=1
                if count==1:print('ready',flush=True)
                return {}
            h.load_config=config;h.owned=lambda *a,**k:object();h.state=lambda vm:'stopped'
            h.api=lambda *a,**k:[{'status':'Stopped'}]
            h.instance_runtime.live_processes=lambda *a:[]
            h.stop=lambda *a,**k:(h.STATE/'effect').write_text('stopped')
            h.run=lambda *a,**k:(h.STATE/'effect').write_text('poweroff')
            sys.argv=['host_runtime.py'];sys.stdin=io.StringIO(payload)
            try:reply=h.main()
            except Exception as error:reply={'kind':'error','data':str(error)}
            print(json.dumps(reply),flush=True)
        """)
        for kind,data in [('drain_guest',{'vm':VM}),('power_off',{'vms':[VM]})]:
            state=Path(self.directory.name)/kind;state.mkdir()
            payload=json.dumps({'kind':kind,'operation_id':OP,'boot_id':BOOT,**data})
            with self.subTest(kind=kind),patch.object(host,'STATE',state),patch.object(host,'shutdown_boot',return_value=BOOT):
                child=None
                try:
                    with (state/'operations.lock').open('a') as lock:
                        fcntl.flock(lock,fcntl.LOCK_EX)
                        child=subprocess.Popen([sys.executable,'-I','-c',script,str(Path(host.__file__)),str(state),payload,BOOT],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
                        self.assertTrue(select.select([child.stdout],[],[],5)[0],'helper did not reach dispatch')
                        self.assertEqual(child.stdout.readline().strip(),'ready')
                        host.cancel_shutdown(OP,BOOT)
                    stdout,stderr=child.communicate(timeout=5)
                    self.assertEqual(child.returncode,0,stderr)
                    reply=json.loads(stdout)
                    message=reply['data']['message'] if reply['kind']=='power_off_rejected' else reply['data']
                    self.assertIn('cancellation',message)
                    self.assertFalse((state/'effect').exists())
                finally:
                    if child is not None and child.poll() is None:child.kill();child.communicate(timeout=5)

if __name__=='__main__':unittest.main()
