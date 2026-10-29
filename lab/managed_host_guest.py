#!/usr/bin/python3
"""Fixed lab guest commands, executed only in a manifest-owned managed host."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

ROOT=Path('/var/lib/homecloud-host')
PROFILE=ROOT/'lab-client.json'

def run(*args,input=None,timeout=60):
    p=subprocess.run(args,input=input,text=True,capture_output=True,timeout=timeout)
    if p.returncode:raise RuntimeError(p.stderr[-1800:] or p.stdout[-1800:])
    return p.stdout

def cli(*args):
    response=json.loads(run('/usr/local/bin/homecloud','--profile',str(PROFILE),*map(str,args)))
    if response['kind']=='error':raise RuntimeError(str(response['data']))
    return response.get('data')

def main():
    action=sys.argv[1]
    if action=='init':
        if not PROFILE.exists():
            # A fresh local authority and local ticket. Neither token, profile
            # nor endpoint secret is emitted by the external test supervisor.
            result=json.loads(run('runuser','-u','homecloud','--','homecloudd','--config','/etc/homecloud/control.json','bootstrap-admin','--name','Nested lab administrator'))
            run('/usr/local/bin/homecloud','--profile',str(PROFILE),'enroll','--ticket','/var/lib/homecloud/control/server.json',input=result['token']+'\n')
        print(json.dumps({'enrolled':True,'owner':cli('status')['me']['id']}))
    elif action=='call':print(json.dumps(cli(*sys.argv[2:])))
    elif action=='try':
        try:print(json.dumps({'accepted':True,'result':cli(*sys.argv[2:])}))
        except RuntimeError as e:print(json.dumps({'accepted':False,'message':str(e)}))
    elif action=='probe':
        arc={line.split()[0]:int(line.split()[-1]) for line in Path('/proc/spl/kstat/zfs/arcstats').read_text().splitlines()[2:]}
        instances=json.loads(run('incus','list','--format=json'))
        vm=[]
        for v in instances:
            if not v['config'].get('user.homecloud.id'):continue
            st=json.loads(run('incus','query','/1.0/instances/'+v['name']+'/state'))
            vm.append({'id':v['config']['user.homecloud.id'],'status':st['status'],'pid':st.get('pid'),
                'boot_autostart':v['config'].get('boot.autostart'),'memory':v['config'].get('limits.memory')})
        swaps={l.split()[0]:int(l.split()[1]) for l in Path('/proc/vmstat').read_text().splitlines() if l.startswith(('pswpin ','pswpout '))}
        print(json.dumps({'boot_id':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
            'memory':int(next(l.split()[1] for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemTotal:')))*1024,
            'arc':{k:arc[k] for k in ('size','c_min','c_max')},'arc_parameters':{k:Path('/sys/module/zfs/parameters/'+k).read_text().strip() for k in ('zfs_arc_max','zfs_arc_min')},
            'versions':{'zfs':Path('/sys/module/zfs/version').read_text().strip(),'incus':run('incus','version').strip(),'kernel':os.uname().release,'qemu':run('qemu-system-x86_64','--version').splitlines()[0]},
            'pool':run('zpool','list','-H','-o','health','hcnested').strip(),'vms':vm,'swap':swaps}))
    else:raise ValueError('Unsupported nested lab command')

if __name__=='__main__':main()
