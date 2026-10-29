import argparse,json,tempfile,unittest
from pathlib import Path
from unittest.mock import Mock
from control_runner import readiness,Runner,GIB

class ReadinessTests(unittest.TestCase):
    def fixture(self,phase='retrying',error='temporary start failure'):
        return {'vms':[{'id':'vm','observed':'pending','error':error}],
                'operations':[{'vm_id':'vm','phase':phase}]}
    def test_retrying_error_is_progress_not_terminal_failure(self):
        self.assertFalse(readiness(self.fixture(),['vm'],'running'))
    def test_explicit_review_is_a_failure_with_retained_resources(self):
        with self.assertRaisesRegex(RuntimeError,'needs review'):
            readiness(self.fixture('needs_review'),['vm'],'running')
    def test_running_observation_does_not_finish_an_active_mutation(self):
        s=self.fixture('applying',None);s['vms'][0]['observed']='running'
        self.assertFalse(readiness(s,['vm'],'running'))
        s['operations'][0]['phase']='succeeded'
        self.assertTrue(readiness(s,['vm'],'running'))
    def test_delete_waits_for_absence_in_the_authenticated_snapshot(self):
        self.assertFalse(readiness(self.fixture(),['vm'],'deleted'))
        self.assertTrue(readiness({'vms':[],'operations':[]},['vm'],'deleted'))

if __name__=='__main__':unittest.main()


class DurableRunnerTests(unittest.TestCase):
    def setUp(self):
        temp=tempfile.TemporaryDirectory();self.addCleanup(temp.cleanup)
        self.path=Path(temp.name)/'proof.json'
    def runner(self):
        r=Runner(argparse.Namespace(state=str(self.path)));self.addCleanup(r.run_lock.close);return r
    def test_concurrent_local_runs_cannot_change_the_same_lab_grants(self):
        first=self.runner()
        with self.assertRaisesRegex(RuntimeError,'Another lab proof'):self.runner()
        first.run_lock.close()
        self.runner()
    def test_resumed_cleanup_observes_existing_delete_instead_of_submitting_it_again(self):
        r=self.runner();grant={'memory_bytes':GIB,'cpu_cores':1,'disk_bytes':20*GIB}
        r.state={'owner':'u','original_grant':grant,'fixtures':[{'id':'v','name':'hc-proof-test'}]}
        r.discover=Mock();r.cli=Mock();r.wait=Mock();r.grant=Mock()
        r.snapshot=Mock(side_effect=[{'vms':[{'id':'v','owner':'u','desired':'deleted','spec':{'name':'hc-proof-test','applications':[]}}]}, {'me':{'grant':grant}}])
        r.cleanup();r.cli.assert_not_called();r.wait.assert_called_once_with(['v'],'deleted')
        self.assertTrue(json.loads(self.path.read_text())['cleaned'])
    def test_cleanup_rejects_a_foreign_resource_before_delete_or_grant_release(self):
        r=self.runner();r.state={'owner':'u','fixtures':[{'id':'v','name':'hc-proof-test'}]}
        r.discover=Mock();r.cli=Mock();r.grant=Mock()
        r.snapshot=Mock(return_value={'vms':[{'id':'v','owner':'other','desired':'running','spec':{'name':'hc-proof-test','applications':[]}}]})
        with self.assertRaises(AssertionError):r.cleanup()
        r.cli.assert_not_called();r.grant.assert_not_called()
