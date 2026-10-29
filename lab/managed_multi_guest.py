#!/usr/bin/python3
"""Multi-user commands; profiles and enrollment secrets stay inside the worker."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import uuid

ROOT=Path('/var/lib/homecloud-host')
PROFILES={label:ROOT/('lab-client.json' if label=='admin' else 'multi-'+label+'.json') for label in ('admin','alice','bob')}
def raw(label,*args):
    p=subprocess.run(['/usr/local/bin/homecloud','--profile',str(PROFILES[label]),*map(str,args)],text=True,capture_output=True,timeout=55)
    if p.returncode:raise RuntimeError(p.stderr[-1500:])
    response=json.loads(p.stdout)
    if response['kind']=='error':raise RuntimeError(str(response['data']))
    if response['kind']=='create_rejected':raise RuntimeError(str(response['data']['message']))
    return response.get('data')
def attempt(label,*args):
    try:return {'accepted':True,'result':raw(label,*args)}
    except RuntimeError as e:return {'accepted':False,'message':str(e)}
def main():
    action=sys.argv[1]
    if action=='users':
        result={}
        for label in ('alice','bob'):
            if not PROFILES[label].exists():
                invitation=raw('admin','invite','Nested '+label,'--memory',3,'--cores',1,'--disk',12)
                p=subprocess.run(['/usr/local/bin/homecloud','--profile',str(PROFILES[label]),'enroll','--ticket','/var/lib/homecloud/control/server.json'],input=invitation['token']+'\n',text=True,capture_output=True,timeout=40)
                assert p.returncode==0,'Local test enrollment failed'
            result[label]=raw(label,'status')['me']['id']
        print(json.dumps(result))
    elif action=='call':print(json.dumps(raw(sys.argv[2],*sys.argv[3:])))
    elif action=='try':print(json.dumps(attempt(sys.argv[2],*sys.argv[3:])))
    elif action=='race':
        ids=json.loads(sys.argv[2]);users=json.loads(sys.argv[3]);gate=threading.Barrier(6)
        jobs=[(label,'start',vid) for label,vid in ids.items() for _ in range(2)]
        jobs += [('admin','grant',uid,'--memory',2,'--cores',1,'--disk',12) for uid in users.values()]
        def submit(job):gate.wait();return {'request':list(job),'response':attempt(*job)}
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:results=list(pool.map(submit,jobs))
        print(json.dumps(results))
    elif action=='proof':
        ids=json.loads(sys.argv[2]);payload=sys.argv[3];write=len(sys.argv)>4 and sys.argv[4]=='write';result={}
        subprocess.run(['systemctl','is-active','--quiet','incus-startup.service'],check=True)
        for label,vid in ids.items():
            name='hc-'+uuid.UUID(vid).hex
            if write:
                code="import os,sys;f=open('/root/multi-user-proof','w');f.write(sys.argv[1]);f.flush();os.fsync(f.fileno());f.close();d=os.open('/root',os.O_RDONLY|os.O_DIRECTORY);os.fsync(d);os.close(d)"
                subprocess.run(['incus','exec',name,'--','python3','-c',code,payload+'-'+label],check=True,capture_output=True)
            else:
                p=subprocess.run(['incus','exec',name,'--','cat','/root/multi-user-proof'],check=True,capture_output=True,text=True);assert p.stdout==payload+'-'+label
            result[label]={'proof_preserved':True}
        print(json.dumps(result))
    elif action=='processes':
        ids=json.loads(sys.argv[2]);result={}
        for label,vid in ids.items():
            name='hc-'+uuid.UUID(vid).hex;pids=[]
            for path in Path('/proc').glob('[0-9]*/cmdline'):
                try:args=path.read_bytes().split(b'\0')
                except OSError:continue
                if args and b'qemu-system' in args[0] and any(name.encode() in a for a in args[1:]):pids.append(int(path.parent.name))
            result[label]=pids
        print(json.dumps(result))
    else:raise ValueError('Unsupported multi-user action')
if __name__=='__main__':main()
