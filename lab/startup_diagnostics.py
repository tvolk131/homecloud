"""Bounded read-only failure capture for verified disposable IDs; no credentials."""
from pathlib import Path
import datetime,json,os,re,sqlite3,subprocess,sys
assert os.geteuid()==0
owner=sys.argv[1];ids=sys.argv[2:];result={'captured_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'vms':{}}
def run(*args):
    try:
        p=subprocess.run(args,capture_output=True,text=True,timeout=15)
        return {'exit':p.returncode,'output':(p.stdout+p.stderr)[-48000:]}
    except subprocess.TimeoutExpired:return {'timeout':True}
with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
    for vid in ids:
        assert re.fullmatch(r'[a-f0-9-]{36}',vid)
        vm=json.loads(db.execute('SELECT record FROM vms WHERE id=?',[vid]).fetchone()[0])
        assert vm['owner']==owner and vm['spec']['name'].startswith('hc-proof-') and not vm['spec'].get('applications')
        name='hc-'+vid.replace('-','');item={'controller':{'desired':vm['desired'],'observed':vm['observed'],'error':vm.get('error')},'state':run('incus','query','/1.0/instances/'+name+'/state'),'qemu_processes':[]}
        group=Path('/sys/fs/cgroup/homecloud.slice/homecloud-'+vid.replace('-','')+'.service/cgroup.procs')
        if group.exists():
            for pid in group.read_text().split():
                proc=Path('/proc')/pid
                try:
                    args=proc.joinpath('cmdline').read_bytes().split(b'\0')
                    names=[args[i+1].decode().split(',')[0] for i in range(len(args)-1) if args[i]==b'-name']
                    if name not in names:continue
                    item['qemu_processes'].append({'pid':pid,'exe':str(proc.joinpath('exe').resolve()),'status':proc.joinpath('status').read_text(),'cgroup':proc.joinpath('cgroup').read_text(),'wchan':proc.joinpath('wchan').read_text()})
                except OSError:continue
        p=run('incus','query','/1.0/instances/'+name)
        try:
            c=json.loads(p['output'])['config'];item['config']={k:c.get(k) for k in ['volatile.vsock_id','volatile.last_state.power','limits.cpu','limits.memory']}
        except (ValueError,KeyError):pass
        for file in ['qemu.log','qemu.conf']:
            path=Path('/var/log/incus')/name/file
            if not path.exists():path=Path('/run/incus')/name/file
            if path.exists():item[file]=path.read_text(errors='replace')[-48000:]
        item['incus_journal']=run('journalctl','-u','incus','--since','-20min','--no-pager','-g',name)
        result['vms'][vid]=item
trace=Path('/var/lib/homecloud/test-runs/startup-soak-observation/syscalls.log')
if trace.exists():
 with trace.open('rb') as f:
  f.seek(max(0,trace.stat().st_size-64000));result['startup_syscall_tail']=f.read(64000).decode('utf-8','replace')
result['incus_version']=run('incus','version');result['qemu_version']=run('qemu-system-x86_64','--version')
result['kernel']=run('uname','-a');result['incus_errors']=run('journalctl','-u','incus','--since','-20min','--no-pager','-p','warning')
result['apparmor']=run('journalctl','-k','--since','-20min','--no-pager','-g','apparmor="DENIED"')
print(json.dumps(result))
