"""Bounded launch investigation through normal Homecloud operations.

Only this manifest's unpaired proof VMs may be rebooted or deleted. Stop on the
first anomaly and retain its fixtures. Never force-stop or weaken host security.
"""
import argparse
import json
from pathlib import Path
import time
from control_runner import Runner, atomic_json
from startup_soak import capture

ROOT = Path(__file__).resolve().parents[1]

GUEST_REBOOT = r'''from pathlib import Path
import json,sqlite3,subprocess,sys,time
owner,vid,prefix=sys.argv[1:]
with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
 vm=json.loads(db.execute('SELECT record FROM vms WHERE id=?',[vid]).fetchone()[0])
assert vm['owner']==owner and vm['spec']['name'].startswith(prefix+'-') and not vm['spec'].get('applications')
name='hc-'+vid.replace('-','')
state=json.loads(subprocess.check_output(['incus','query','/1.0/instances/'+name+'/state'],text=True,timeout=10))
assert state['status']=='Running' and state['pid']>0
before=state['pid'];begin=time.monotonic_ns()
p=subprocess.run(['incus','exec',name,'--','systemctl','reboot'],capture_output=True,text=True,timeout=20)
assert p.returncode in (0,255),p.stderr[-2000:]
print(json.dumps({'before_pid':before,'requested_ns':begin,'exit':p.returncode}))
'''

WAIT_PROCESS = r'''import json,sqlite3,subprocess,sys,time
from pathlib import Path
owner,vid,prefix,old=sys.argv[1:];old=int(old)
with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
 vm=json.loads(db.execute('SELECT record FROM vms WHERE id=?',[vid]).fetchone()[0])
assert vm['owner']==owner and vm['spec']['name'].startswith(prefix+'-') and not vm['spec'].get('applications')
name='hc-'+vid.replace('-','');deadline=time.monotonic()+40
while time.monotonic()<deadline:
 p=subprocess.run(['incus','query','/1.0/instances/'+name+'/state'],capture_output=True,text=True,timeout=8)
 s=json.loads(p.stdout) if p.returncode==0 else {}
 pid=s.get('pid',0)
 if s.get('status')=='Running' and pid>0 and pid!=old:
  print(json.dumps({'pid':pid,'ns':time.monotonic_ns()}));break
 time.sleep(.25)
else:raise TimeoutError('Expected running process generation was not observed')
'''

WAIT_AGENT = r'''import json,sqlite3,subprocess,sys,time
owner,vid,prefix=sys.argv[1:]
with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
 vm=json.loads(db.execute('SELECT record FROM vms WHERE id=?',[vid]).fetchone()[0])
assert vm['owner']==owner and vm['spec']['name'].startswith(prefix+'-') and not vm['spec'].get('applications')
name='hc-'+vid.replace('-','');deadline=time.monotonic()+60
code='import fcntl,json,os,struct;f=os.open("/dev/vsock",os.O_RDONLY);print(json.dumps({"guest_cid":struct.unpack("I",fcntl.ioctl(f,0x7b9,struct.pack("I",0)))[0]}));os.close(f)'
while time.monotonic()<deadline:
 try:p=subprocess.run(['incus','exec',name,'--','python3','-c',code],capture_output=True,text=True,timeout=4)
 except subprocess.TimeoutExpired:continue
 if p.returncode==0:
  guest=json.loads(p.stdout)
  config=json.loads(subprocess.check_output(['incus','query','/1.0/instances/'+name],text=True,timeout=8))['config']
  assert str(guest['guest_cid'])==config['volatile.vsock_id']
  print(json.dumps(guest));break
 time.sleep(1)
else:raise TimeoutError('Guest agent did not recover after reboot')
'''


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--state', required=True); p.add_argument('--ssh-config', required=True)
    p.add_argument('--cli', default=str(ROOT/'target/debug/homecloud'))
    p.add_argument('--ssh-key', default=str(Path.home()/'.ssh/id_ed25519.pub'))
    p.add_argument('--cycles', type=int, default=12)
    p.add_argument('--max-seconds', type=int, default=5400)
    p.add_argument('--cleanup', action='store_true')
    a = p.parse_args(); assert 1 <= a.cycles <= 20 and 120 <= a.max_seconds <= 5400
    r = Runner(a)
    if a.cleanup: r.cleanup(); return
    r.start_run(); r.state.update(protocol='launch-reproduction-v2', cycles=[], started_at=time.time()); r.save()
    print(json.dumps({'event': 'ready_for_observer', 'owner': r.state['owner'], 'prefix': r.state['prefix']}), flush=True)
    # The parent starts the private trace/witness before releasing this gate.
    gate = r.path.with_suffix('.observer-ready')
    deadline = time.monotonic()+120
    while not gate.exists():
        if time.monotonic() >= deadline: raise TimeoutError('Observer was not ready')
        time.sleep(.25)
    began = time.monotonic()
    try:
        for cycle in range(a.cycles):
            if time.monotonic()-began >= a.max_seconds: raise TimeoutError('Campaign time bound reached')
            start = time.monotonic(); ids = r.create_batch(3); r.wait(ids, 'running')
            event = {'cycle': cycle+1, 'created': ids, 'creation_seconds': round(time.monotonic()-start, 2)}
            # Release/readmit compute through the same admission path users use.
            vid = ids[0]; r.cli('stop', vid); r.wait([vid], 'stopped')
            v = next(v for v in r.snapshot()['vms'] if v['id'] == vid)
            assert v['resources']['memory_bytes'] == 0 and v['resources']['cpu_cores'] == 0
            r.cli('start', vid); r.wait([vid], 'running'); event['stop_start'] = vid
            # Exercise the Incus internal restart path, distinct from API stop/start.
            vid = ids[1]
            reboot = r.remote(GUEST_REBOOT, r.state['owner'], vid, r.state['prefix'])
            # A duplicate desired-Running request must retain existing claims
            # while Incus handles the guest-initiated reboot.
            repeated = r.cli('start', vid); reboot['api_start_phase'] = repeated['phase']
            assert repeated['phase'] == 'succeeded', 'Repeated Running intent enqueued a physical launch'
            reboot['after'] = r.remote(WAIT_PROCESS, r.state['owner'], vid, r.state['prefix'], reboot['before_pid'], timeout=50)
            reboot['agent'] = r.remote(WAIT_AGENT, r.state['owner'], vid, r.state['prefix'], timeout=75)
            r.wait([vid], 'running'); event['guest_reboot'] = {'id': vid, **reboot}
            r.state['cycles'].append(event); r.save()
            r.cleanup(); r.state.pop('cleaned', None); r.save()
            # Cancel once an actual QEMU generation is running, while Create may
            # still be waiting on the guest agent/cloud-init.
            vid = r.create_batch(1)[0]
            launched = r.remote(WAIT_PROCESS, r.state['owner'], vid, r.state['prefix'], 0, timeout=50)
            snapshot = r.snapshot()
            event['timed_cancel'] = {'id': vid, 'running_process': launched,
                                    'active_create': any(o['vm_id'] == vid and o['phase'] not in ('succeeded', 'failed') for o in snapshot['operations'])}
            r.cli('delete', vid); r.wait([vid], 'deleted'); r.cleanup(); r.state.pop('cleaned', None); r.save()
            if cycle % 2 == 0:
                vid = r.create_batch(1)[0]; r.cli('delete', vid); r.wait([vid], 'deleted')
                event['immediate_cancel'] = vid
                r.cleanup(); r.state.pop('cleaned', None); r.save()
            event['seconds'] = round(time.monotonic()-start, 2); r.save()
            print(json.dumps(event), flush=True)
        r.cleanup(); r.state.update(passed=True, finished_at=time.time()); r.save()
        print(json.dumps({'event': 'passed', 'cycles': len(r.state['cycles'])}), flush=True)
    except BaseException as error:
        r.state['failure'] = str(error); r.save(); capture(r)
        print(json.dumps({'event': 'stopped_on_anomaly', 'error': str(error)}), flush=True)
        raise


if __name__ == '__main__': main()
