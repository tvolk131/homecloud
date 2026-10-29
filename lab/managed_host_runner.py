#!/usr/bin/python3
"""External supervisor for a full managed Homecloud host VM.

The manifest records disposable IDs and immutable request keys before RPC. The
outer controller owns worker memory/disk/CPU; the inner controller owns L2 VMs.
No original devbox, provider identity, production pool or SQLite DB is reused.
"""
import argparse
import json
from pathlib import Path
import shlex
import subprocess
import time
import uuid
from control_runner import Runner,atomic_json,GIB

class ManagedRunner(Runner):
    def start(self):
        if not self.state:self.start_run()
        self.state['managed_host']=True
        if not self.state['fixtures']:
            self.state['fixtures']=[{'name':self.state['prefix']+'-host','request_id':str(uuid.uuid4()),'id':None,'memory':getattr(self.args,'worker_memory',8),'cores':getattr(self.args,'worker_cores',2)}];self.save()
        e=self.state['fixtures'][0]
        self.grant(e['memory']+4,e.get('cores',2)+1)
        if not e['id']:
            op=self.cli('create',e['name'],'--memory',e['memory'],'--cores',e.get('cores',2),'--disk',32,'--ssh-key',self.args.ssh_key,'--homecloud-host','--request-id',e['request_id'])
            e['id']=op['vm_id'];self.save()
        return e['id']
    def guest(self,code,*args,timeout=60):
        e=self.state['fixtures'][0];assert e['id']
        wrapper='''import json,subprocess,sys,uuid,sqlite3
vid,owner,expected_name=sys.argv[1:4]
with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
 vm=json.loads(db.execute('SELECT record FROM vms WHERE id=?',[vid]).fetchone()[0])
assert vm['owner']==owner and vm['spec']['name']==expected_name and expected_name.startswith('hc-proof-') and vm['spec']['applications']==['homecloud_host']
name='hc-'+uuid.UUID(vid).hex
instance=json.loads(subprocess.check_output(['incus','query','/1.0/instances/'+name]))
assert instance['config'].get('user.homecloud.id')==vid and instance['config'].get('user.homecloud.owner')==owner
source=sys.stdin.read()
p=subprocess.run(['incus','exec',name,'--','python3','-',*sys.argv[4:]],input=source,capture_output=True,text=True,timeout='''+str(timeout)+''')
if p.returncode:raise RuntimeError(p.stderr[-3000:] or p.stdout[-3000:])
print(p.stdout)
'''
        # Runner.remote sends source on stdin, so encode the fixed guest program
        # inside a wrapper. Arguments remain separate, never shell interpolated.
        wrapper=wrapper.replace('source=sys.stdin.read()', 'source='+repr(code))
        return self.remote(wrapper,e['id'],self.state['owner'],e['name'],*args,timeout=timeout+15)
    def peek(self,ids):
        e=self.state['fixtures'][0];assert ids==[e['id']]
        return self.remote(Path(__file__).with_name('managed_host_probe.py').read_text(),self.state['owner'],e['name'],*ids)
    def install_status(self):
        s=self.snapshot();vid=self.state['fixtures'][0]['id']
        vm=next(v for v in s['vms'] if v['id']==vid)
        app=next((a for a in s['applications'] if a['vm_id']==vid and a['kind']=='homecloud_host'),None)
        return {'vm':{k:vm[k] for k in ('id','desired','observed','error')},'application':app}
    def cleanup(self):
        self.discover();s=self.snapshot();by_id={v['id']:v for v in s['vms']}
        ids=[]
        for e in self.state['fixtures']:
            if e['id'] not in by_id:continue
            vm=by_id[e['id']];assert vm['owner']==self.state['owner'] and vm['spec']['name']==e['name'] and vm['spec']['applications']==['homecloud_host']
            ids.append(vm['id'])
            if vm['desired']!='deleted':self.cli('delete',vm['id'])
        self.wait(ids,'deleted')
        g=self.state['original_grant'];self.grant(g['memory_bytes']//GIB,g['cpu_cores'])
        assert self.snapshot()['me']['grant']==g;self.state['cleaned']=True;self.save()


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['boot','status','cleanup'])
    p.add_argument('--state',required=True);p.add_argument('--ssh-config',required=True)
    p.add_argument('--cli',default=str(Path(__file__).resolve().parents[1]/'target/debug/homecloud'))
    p.add_argument('--worker-memory',type=int,default=8);p.add_argument('--worker-cores',type=int,default=2)
    p.add_argument('--ssh-key',default=str(Path.home()/'.ssh/id_ed25519.pub'))
    args=p.parse_args();r=ManagedRunner(args)
    if args.action=='boot':print(json.dumps({'vm_id':r.start()}))
    elif args.action=='status':print(json.dumps(r.install_status()))
    else:r.cleanup();print(json.dumps({'cleaned':True}))

if __name__=='__main__':main()
