from pathlib import Path
import importlib.util,json,subprocess,tempfile,unittest
from types import SimpleNamespace
from unittest.mock import Mock,patch

spec=importlib.util.spec_from_file_location('instance_runtime_tests',Path(__file__).parents[1]/'instance_runtime.py')
r=importlib.util.module_from_spec(spec);spec.loader.exec_module(r)
VM={'id':'00000000-0000-4000-8000-000000000001','owner':'owner','desired':'running'}

class InstanceRuntimeTests(unittest.TestCase):
    def setUp(self):
        patcher=patch.object(r,'boot_id',return_value='test-boot');patcher.start();self.addCleanup(patcher.stop)
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);self.root=Path(t.name)
        self.host=SimpleNamespace(STATE=self.root,vm_name=lambda v:'hc-'+v['id'].replace('-',''),
            state=Mock(return_value='stopped'),api=Mock(return_value={}),
            run=Mock(return_value=subprocess.CompletedProcess([],0,stderr='')),
            save=lambda p,v:p.write_text(json.dumps(v)),owned=Mock(return_value={'config':{}}))
    def test_existing_qemu_with_stopped_api_state_never_issues_another_start(self):
        with patch.object(r,'live_processes',return_value=[123]),patch.object(r,'wait',side_effect=RuntimeError('pending: existing startup')):
            with self.assertRaisesRegex(RuntimeError,'pending: existing'):r.start(self.host,VM)
        self.host.run.assert_not_called()
    def test_first_boot_reboot_without_a_public_operation_cannot_launch_again(self):
        r.launch_path(self.host,VM).write_text(json.dumps({'boot':'test-boot'}))
        with patch.object(r,'live_processes',return_value=[]):
            with self.assertRaisesRegex(RuntimeError,'pending:.*first-boot reboot'):r.start(self.host,VM)
        self.host.run.assert_not_called();self.host.api.assert_not_called()
    def test_incus_start_operation_is_allowed_to_finish_without_duplicate_start(self):
        self.host.api.return_value={'running':[{'status_code':103,'resources':{'instances':['/1.0/instances/'+self.host.vm_name(VM)]}}]}
        with patch.object(r,'live_processes',return_value=[]),patch.object(r,'wait',side_effect=RuntimeError('pending: operation')):
            with self.assertRaisesRegex(RuntimeError,'pending: operation'):r.start(self.host,VM)
        self.host.run.assert_not_called()
    def test_lost_start_response_with_live_qemu_is_reobserved(self):
        self.host.run.return_value=subprocess.CompletedProcess([],1,stderr='lost ack')
        with patch.object(r,'live_processes',side_effect=[[],[321]]),patch.object(r,'wait',side_effect=RuntimeError('pending: existing startup')):
            with self.assertRaisesRegex(RuntimeError,'pending: existing'):r.start(self.host,VM)
        self.assertEqual(self.host.run.call_count,1)
    def test_wait_deadline_survives_process_restart_and_explicit_retry_gets_fresh_wait(self):
        with patch.object(r.time,'monotonic',return_value=10):
            with self.assertRaisesRegex(RuntimeError,'pending:'):r.wait(self.host,VM,'agent','agent unavailable',20)
        with patch.object(r.time,'monotonic',return_value=31):
            with self.assertRaisesRegex(RuntimeError,'conflict:'):r.wait(self.host,VM,'agent','agent unavailable',20)
        with patch.object(r.time,'monotonic',return_value=32):
            with self.assertRaisesRegex(RuntimeError,'pending:'):r.wait(self.host,VM,'agent','agent unavailable',20)
    def test_delete_fence_blocks_delayed_create_after_physical_cleanup(self):
        r.fence(self.host,{**VM,'desired':'deleted'})
        # Replay is safe; a delayed old start/create cannot resurrect the ID.
        r.fence(self.host,{**VM,'desired':'deleted'})
        with self.assertRaisesRegex(AssertionError,'stale apply'):r.fence(self.host,VM)
        with self.assertRaisesRegex(AssertionError,'ownership'):r.fence(self.host,{**VM,'owner':'another','desired':'deleted'})



    def test_setup_stage_preserves_start_time_and_completion_clears_it(self):
        with patch.object(r.time,'time',return_value=100):r.checkpoint(self.host,VM,'guest_setup')
        with patch.object(r.time,'time',return_value=120):r.checkpoint(self.host,VM,'guest_setup')
        self.assertEqual(r.progress(self.host,VM),{'stage':'guest_setup','since':100})
        r.finished(self.host,VM);self.assertIsNone(r.progress(self.host,VM))
    def test_logs_require_confirmed_ownership_before_any_guest_access(self):
        self.host.owned.side_effect=AssertionError('foreign instance')
        with self.assertRaisesRegex(AssertionError,'foreign'):r.diagnostics(self.host,VM)
        self.host.run.assert_not_called()
    def test_unavailable_guest_agent_has_a_bounded_diagnostic_result(self):
        self.host.state.return_value='running'
        self.host.run.side_effect=subprocess.TimeoutExpired('incus',8)
        result=r.diagnostics(self.host,VM)
        self.assertFalse(result['logs']);self.assertTrue(any('eight seconds' in line for line in result['checks']))
        self.assertEqual(self.host.run.call_args.kwargs['timeout'],8)

    def test_actual_guest_reader_compiles_bounds_logs_and_refuses_fifo_files(self):
        import os,sys
        output=self.root/'output';output.write_text('x'*16000)
        fifo=self.root/'fifo';os.mkfifo(fifo)
        self.host.state.return_value='running'
        def execute_reader(*args,**kwargs):
            code=args[-1].replace('/var/log/cloud-init-output.log',str(output)).replace('/var/log/cloud-init.log',str(fifo))
            return subprocess.run([sys.executable,'-c',code],capture_output=True,text=True,timeout=3)
        self.host.run.side_effect=execute_reader
        result=r.diagnostics(self.host,VM)
        self.assertEqual(len(result['logs']),2)
        self.assertEqual(result['logs'][0]['text'],'[Earlier output omitted]\n'+'x'*12000)
        self.assertEqual(result['logs'][1]['text'],'Log unavailable')

    def test_start_error_survives_wait_guard_expiry_and_is_available_in_diagnostics(self):
        self.host.run.return_value=subprocess.CompletedProcess([],1,stderr='mkisofs is missing')
        with patch.object(r,'live_processes',return_value=[]):
            with self.assertRaisesRegex(RuntimeError,'mkisofs'):r.start(self.host,VM)
            with patch.object(r,'wait',side_effect=RuntimeError('conflict: launch wait expired')):
                with self.assertRaisesRegex(RuntimeError,'wait expired'):r.start(self.host,VM)
            self.assertFalse(r.launch_path(self.host,VM).exists())
            result=r.diagnostics(self.host,VM)
        self.assertTrue(any('mkisofs' in c for c in result['checks']))
        self.assertEqual(self.host.run.call_count,1)
        r.finished(self.host,VM)
        self.assertFalse(r.launch_error_path(self.host,VM).exists())
