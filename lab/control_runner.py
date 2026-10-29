"""Resumable proofs through the real control API; only recorded run IDs are disposable.

Run with --ssh-config pointing at the pinned lab SSH config. A failed proof keeps
its manifest and leases for inspection; `cleanup` uses the normal delete API.
"""
import argparse
import concurrent.futures
import fcntl
import json
import os
from pathlib import Path
import shlex
import subprocess
import time
import uuid

GIB = 1024**3
TERMINAL = {'succeeded', 'failed'}


def atomic_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_suffix('.tmp')
    with temp.open('w') as f:
        json.dump(value, f, indent=2); f.write('\n'); f.flush(); os.fsync(f.fileno())
    temp.chmod(0o600); temp.replace(path)
    fd = os.open(path.parent, os.O_RDONLY)
    try: os.fsync(fd)
    finally: os.close(fd)


def readiness(snapshot, ids, desired):
    """Retry transport/provisioning progress; report explicit review separately."""
    by_id = {v['id']: v for v in snapshot['vms']}
    for vid in ids:
        ops = [o for o in snapshot['operations'] if o['vm_id'] == vid and o['phase'] not in TERMINAL]
        if any(o['phase'] == 'needs_review' for o in ops):
            raise RuntimeError('VM '+vid+' needs review: '+str(by_id.get(vid, {}).get('error')))
    if desired == 'deleted': return all(vid not in by_id for vid in ids)
    return all(vid in by_id and by_id[vid]['observed'] == desired and not by_id[vid].get('error')
               and not any(o['vm_id'] == vid and o['phase'] not in TERMINAL for o in snapshot['operations']) for vid in ids)


class Runner:
    def __init__(self, args):
        self.args = args; self.path = Path(args.state)
        self.path.parent.mkdir(parents=True,exist_ok=True)
        self.run_lock=(self.path.parent/'.homecloud-lab.lock').open('a')
        try:fcntl.flock(self.run_lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:
            self.run_lock.close()
            raise RuntimeError('Another lab proof is running; finish it before changing grants')
        self.state = json.loads(self.path.read_text()) if self.path.exists() else None
        if self.state and self.state.get('version',1)!=1:raise RuntimeError('Unsupported manifest version')
    def save(self): atomic_json(self.path, self.state)
    def cli(self, *args):
        p = subprocess.run([self.args.cli, *map(str, args)], capture_output=True, text=True, timeout=55)
        if p.returncode: raise RuntimeError('Control transport failed: '+p.stderr.strip())
        r = json.loads(p.stdout)
        if r['kind'] == 'error': raise RuntimeError(str(r['data']))
        return r.get('data')
    def snapshot(self): return self.cli('status')
    def remote(self, code, *args, timeout=60):
        compile(code,'<lab-host-helper>','exec')
        command = shlex.join(['sudo','-n','python3','-', *map(str,args)])
        p = subprocess.run(['ssh','-F',self.args.ssh_config,'-o','BatchMode=yes','hclab-host',command],
                           input=code,capture_output=True,text=True,timeout=timeout)
        if p.returncode: raise RuntimeError('Lab helper failed: '+p.stderr.strip())
        return json.loads(p.stdout)
    def start_run(self):
        if self.state: raise RuntimeError('Existing manifest: resume or clean up that run first')
        s = self.snapshot(); me = s['me']; token = uuid.uuid4().hex
        assert me['role'] == 'admin'
        self.state = {'version':1, 'run_id':token, 'owner':me['id'], 'prefix':'hc-proof-'+token[:10],
                      'original_grant':me['grant'], 'gpu_allowed':me['gpu_allowed'],
                      'fixtures':[], 'observations':[], 'passed':False}
        self.save()
    def grant(self, memory, cores):
        g = self.state['original_grant']
        args = ['grant',self.state['owner'],'--memory',memory,'--cores',cores,'--disk',g['disk_bytes']//GIB]
        if self.state['gpu_allowed']: args.append('--gpu')
        self.cli(*args)
    def create_batch(self, count, memory=1, disk=10):
        s = self.snapshot(); used = s['me']['committed']
        self.grant(max(self.state['original_grant']['memory_bytes']//GIB, used['memory_bytes']//GIB+count*memory),
                   max(self.state['original_grant']['cpu_cores'], used['cpu_cores']+count))
        entries = []
        for i in range(count):
            e = {'name':self.state['prefix']+'-'+str(len(self.state['fixtures'])+1),
                 'request_id':str(uuid.uuid4()), 'id':None, 'memory':memory}
            self.state['fixtures'].append(e); entries.append(e)
        self.save()  # Keys and names survive a lost create acknowledgement.
        def create(e):
            op = self.cli('create',e['name'],'--memory',memory,'--cores',1,'--disk',disk,
                          '--ssh-key',self.args.ssh_key,'--request-id',e['request_id'])
            return e, op['vm_id']
        with concurrent.futures.ThreadPoolExecutor(max_workers=count) as pool:
            for future in concurrent.futures.as_completed([pool.submit(create,e) for e in entries]):
                e, vid = future.result(); e['id'] = vid; self.save()
        return [e['id'] for e in entries]
    def discover(self):
        s = self.snapshot()
        for e in self.state['fixtures']:
            if e['id'] is None:
                matches = [v for v in s['vms'] if v['owner']==self.state['owner'] and v['spec']['name']==e['name']]
                assert len(matches)<=1
                if matches:e['id']=matches[0]['id']
        self.save(); return s
    def peek(self, ids):
        code = (Path(__file__).parent/'control_probe.py').read_text()
        return self.remote(code, self.state['owner'], *ids)
    def wait(self, ids, desired, timeout=480):
        deadline=time.monotonic()+timeout; last_peek=0
        while time.monotonic()<deadline:
            s=self.snapshot()
            if desired!='deleted' and time.monotonic()-last_peek>=8:
                observation=self.peek(ids); self.state['observations'].append(observation);self.save();last_peek=time.monotonic()
                if any(v.get('cid_mismatch') for v in observation['vms'].values()):
                    raise RuntimeError('VSOCK mismatch captured; leases and fixtures retained for inspection')
            if readiness(s,ids,desired):return s
            time.sleep(2)
        raise TimeoutError('Timed out waiting for '+desired+'; manifest retained')
    def cleanup(self):
        self.discover(); ids=[e['id'] for e in self.state['fixtures'] if e['id']]
        s=self.snapshot(); by_id={v['id']:v for v in s['vms']}
        for e in self.state['fixtures']:
            if e['id'] in by_id:
                v=by_id[e['id']]
                assert v['owner']==self.state['owner'] and v['spec']['name']==e['name'] and not v['spec'].get('applications')
                # A resumed cleanup observes its durable delete instead of
                # issuing another mutation against an already-active operation.
                if v['desired']!='deleted':self.cli('delete',e['id'])
        self.wait(ids,'deleted')
        g=self.state['original_grant']; self.grant(g['memory_bytes']//GIB,g['cpu_cores'])
        assert self.snapshot()['me']['grant']==g
        self.state['cleaned']=True;self.save()


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('action',choices=['boot','resume','cleanup'])
    p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True)
    p.add_argument('--cli',default=str(Path(__file__).resolve().parents[1]/'target/debug/homecloud'))
    p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'))
    p.add_argument('--parallel',type=int,choices=[1,2,3],default=3)
    p.add_argument('--cycles',type=int,default=2)
    args=p.parse_args(); r=Runner(args)
    if args.action=='cleanup':r.cleanup();return
    if args.action=='resume':
        r.discover();r.wait([e['id'] for e in r.state['fixtures'] if e['id']],'running');r.cleanup();return
    r.start_run()
    try:
        for cycle in range(args.cycles):
            ids=r.create_batch(args.parallel);print('Created batch '+str(cycle+1),flush=True)
            r.wait(ids,'running');r.cleanup()
            r.state.pop('cleaned',None);r.save()
        r.state['passed']=True;r.save();print('Fresh provisioning cycles passed',flush=True)
    except BaseException as error:
        r.state['failure']=str(error);r.save();print('Proof stopped; inspect or resume using '+args.state,flush=True);raise

if __name__=='__main__':main()
