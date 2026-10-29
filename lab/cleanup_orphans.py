"""Explicit operator cleanup for manifest-selected, deleted-intent proof guests only.
Never imported by the production adapter; no force is an automatic retry policy.
"""
from pathlib import Path
import json,os,re,signal,sqlite3,subprocess,sys,time,uuid
assert os.geteuid()==0
result=[]
with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
 for vid in sys.argv[1:]:
  vid=str(uuid.UUID(vid));v=json.loads(db.execute('SELECT record FROM vms WHERE id=?',[vid]).fetchone()[0])
  assert v['desired']=='deleted' and v['spec']['name'].startswith('hc-proof-') and not v['spec'].get('applications')
  name='hc-'+uuid.UUID(vid).hex
  p=subprocess.run(['incus','query','/1.0/instances/'+name+'/state'],capture_output=True,text=True)
  if not p.returncode:assert json.loads(p.stdout)['status']=='Stopped','Tracked VM must finish stopping first'
  else:assert 'not found' in p.stderr.lower(),'Cannot prove Incus state'
  group=Path('/sys/fs/cgroup/homecloud.slice')/('homecloud-'+uuid.UUID(vid).hex+'.service')
  for raw in (group/'cgroup.procs').read_text().split() if group.exists() else []:
   pid=int(raw);proc=Path('/proc')/raw
   try:
    args=proc.joinpath('cmdline').read_bytes().split(b'\0')
    if b'-name' not in args:continue
    assert args[args.index(b'-name')+1].decode().split(',')[0]==name
    assert proc.joinpath('exe').resolve().name=='qemu-system-x86_64'
    assert group.name in proc.joinpath('cgroup').read_text()
    fd=os.pidfd_open(pid);signal.pidfd_send_signal(fd,signal.SIGTERM);os.close(fd);result.append({'id':vid,'pid':pid})
   except FileNotFoundError:continue
print(json.dumps({'explicit_disposable_cleanup':result}))
