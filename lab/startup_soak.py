"""Full Incus lifecycle soak through Homecloud; failure evidence is private.

Only manifest-recorded, unpaired proof VMs are touched. No automatic force stop,
process kill, permission change, or retry of a reviewed production operation.
"""
import argparse,json,time,traceback
from pathlib import Path
from control_runner import Runner,atomic_json

ROOT=Path(__file__).resolve().parents[1]

def capture(r):
    ids=[e['id'] for e in r.state['fixtures'] if e.get('id')]
    try:
        atomic_json(r.path.with_suffix('.failure.json'),r.remote((ROOT/'lab/startup_diagnostics.py').read_text(),r.state['owner'],*ids,timeout=90))
    except Exception as error:
        r.state['diagnostic_error']=str(error);r.save()

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True)
    p.add_argument('--cli',default=str(ROOT/'target/debug/homecloud'))
    p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'))
    p.add_argument('--cycles',type=int,default=17);p.add_argument('--parallel',type=int,default=3,choices=[1,2,3])
    p.add_argument('--cleanup',action='store_true')
    a=p.parse_args();r=Runner(a)
    if a.cleanup:r.cleanup();return
    r.start_run();r.state.update(protocol='startup-soak-v1',cycles=[],started_at=time.time());r.save()
    try:
        for cycle in range(a.cycles):
            start=time.monotonic();ids=r.create_batch(a.parallel);r.wait(ids,'running')
            observation={'cycle':cycle+1,'created':ids,'creation_seconds':round(time.monotonic()-start,2)}
            # Exercise release and new admission using the normal API.
            vid=ids[cycle%len(ids)];r.cli('stop',vid);r.wait([vid],'stopped')
            s=r.snapshot();vm=next(v for v in s['vms'] if v['id']==vid)
            assert vm['resources']['memory_bytes']==0 and vm['resources']['cpu_cores']==0
            r.cli('start',vid);r.wait([vid],'running');observation['stop_start']=vid
            r.state['cycles'].append(observation);r.save()
            r.cleanup();r.state.pop('cleaned',None);r.save()
            # Cancel immediately after durable admission, allowing either
            # pre-launch cancellation or cancellation during physical setup.
            if cycle%3==0:
                cancelled=r.create_batch(1)[0];r.cli('delete',cancelled);r.wait([cancelled],'deleted')
                observation['cancelled_create']=cancelled;r.save();r.cleanup();r.state.pop('cleaned',None);r.save()
            print(json.dumps(observation),flush=True)
        r.cleanup();r.state.update(passed=True,finished_at=time.time());r.save()
    except BaseException as error:
        r.state['failure']=str(error);r.save();capture(r);raise

if __name__=='__main__':main()
