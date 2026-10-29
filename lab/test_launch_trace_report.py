import unittest
from launch_trace_report import analyze

class LaunchTraceTests(unittest.TestCase):
    def test_background_child_is_normal_after_acknowledged_parent_exit(self):
        text = '''exec ns=1 pid=10 tid=10 cg=20 file=/usr/bin/qemu-system-x86_64
fork ns=2 pid=10 tid=10 parent=10 child=11 cg=20
fork ns=3 pid=11 tid=11 parent=11 child=12 cg=20
io-enter ns=4 pid=10 tid=10 cg=20 op=sys_enter_read fd=7 ino=50 mode=4480
io-enter ns=5 pid=12 tid=12 cg=20 op=sys_enter_write fd=8 ino=50 mode=4480
io-return ns=6 pid=12 tid=12 op=sys_exit_write fd=8 ret=1
io-return ns=7 pid=10 tid=10 op=sys_exit_read fd=7 ret=1
exit ns=8 pid=10 tid=10 cg=20 raw_status=0 group_dead=1'''
        report = analyze(text, [{'launcher_pid': 10, 'vm': 'owned-vm'}])
        self.assertEqual(report['summary']['readiness_acknowledged'], 1)
        self.assertEqual(report['summary']['matched_pipe_writes'], 1)

    def test_failed_second_launch_is_distinct_from_the_existing_generation(self):
        text = '''exec ns=1 pid=10 tid=10 cg=20 file=/usr/bin/qemu-system-x86_64
fork ns=2 pid=10 tid=10 parent=10 child=11 cg=20
exit ns=3 pid=10 tid=10 cg=20 raw_status=0 group_dead=1
exec ns=4 pid=30 tid=30 cg=20 file=/usr/bin/qemu-system-x86_64
fork ns=5 pid=30 tid=30 parent=30 child=31 cg=20
io-enter ns=6 pid=30 tid=30 cg=20 op=sys_enter_read fd=7 ino=80 mode=4480
exit ns=7 pid=31 tid=31 cg=20 raw_status=256 group_dead=1
io-return ns=8 pid=30 tid=30 op=sys_exit_read fd=7 ret=0
exit ns=9 pid=30 tid=30 cg=20 raw_status=256 group_dead=1'''
        report = analyze(text, [{'launcher_pid': p, 'vm': 'owned-vm'} for p in [10, 30]])
        self.assertEqual(len(report['launches']), 2)
        failed = report['launches'][1]
        self.assertEqual(failed['parent_raw_status'], 256)
        self.assertEqual([e['child'] for e in failed['forks']], [31])
        self.assertFalse(failed['readiness_acknowledged'])

    def test_one_byte_return_without_successful_parent_exit_is_not_success(self):
        text = '''exec ns=1 pid=10 tid=10 cg=20 file=/usr/bin/qemu-system-x86_64
io-enter ns=2 pid=10 tid=10 cg=20 op=sys_enter_read fd=7 ino=50 mode=4480
io-return ns=3 pid=10 tid=10 op=sys_exit_read fd=7 ret=1
exit ns=4 pid=10 tid=10 cg=20 raw_status=9 group_dead=1'''
        report = analyze(text, [{'launcher_pid': 10, 'vm': 'owned-vm'}])
        self.assertFalse(report['launches'][0]['readiness_acknowledged'])

if __name__ == '__main__': unittest.main()
