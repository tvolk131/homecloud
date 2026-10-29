"""Read-only diagnostics for manifest-recorded disposable VM IDs."""
from pathlib import Path
import json,os,re,sqlite3,subprocess,sys
assert os.geteuid()==0
owner=sys.argv[1];ids=sys.argv[2:]
def run(*a):return subprocess.run(a,capture_output=True,text=True,timeout=10)
result={'vms':{}}
with sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro',uri=True) as db:
    for vid in ids:
        assert re.fullmatch(r'[a-f0-9-]{36}',vid)
        vm=json.loads(db.execute('SELECT record FROM vms WHERE id=?',[vid]).fetchone()[0])
        assert vm['owner']==owner and vm['spec']['name'].startswith('hc-proof-') and not vm['spec'].get('applications')
        name='hc-'+vid.replace('-','');p=run('incus','query','/1.0/instances/'+name)
        if p.returncode:result['vms'][vid]={'present':False};continue
        instance=json.loads(p.stdout);config=instance['config'];assert config['user.homecloud.id']==vid and config['user.homecloud.owner']==owner
        probe=run('incus','query','/1.0/instances/'+name+'/state')
        try:
            assert probe.returncode==0
            state=json.loads(probe.stdout)
            assert isinstance(state,dict) and state.get('status')
        except (AssertionError,ValueError):
            result['vms'][vid]={'present':True,'state_probe_unavailable':probe.stderr[-500:] or 'Empty or invalid Incus state during transition'}
            continue
        qconf=Path('/run/incus',name,'qemu.conf')
        try:match=re.search(r'guest-cid = "(\d+)"',qconf.read_text())
        except OSError:match=None
        expected=config.get('volatile.vsock_id');actual=match.group(1) if match else None
        result['vms'][vid]={'present':True,'status':state['status'],'pid':state.get('pid'),
            'recorded_cid':expected,'generated_qemu_cid':actual,'cid_mismatch':bool(state['status']=='Running' and state.get('pid') and expected and actual and expected!=actual),
            'controller_state':vm['observed'],'controller_error':vm.get('error')}
print(json.dumps(result))
