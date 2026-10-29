"""Read-only, bounded process-generation witness for one unpaired proof prefix."""
import argparse
from contextlib import closing
import http.client
import json
import os
from pathlib import Path
import re
import signal
import socket
import sqlite3
import time


class Incus(http.client.HTTPConnection):
    def __init__(self): super().__init__('localhost', timeout=2)
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect('/var/lib/incus/unix.socket')


def api(path):
    client = Incus()
    try:
        client.request('GET', path)
        r = client.getresponse()
        body = r.read(2*1024**2)
        if r.status == 404: return None
        assert r.status == 200, (r.status, path)
        return json.loads(body)['metadata']
    finally: client.close()


def processes(vid):
    group = Path('/sys/fs/cgroup/homecloud.slice/homecloud-'+vid.replace('-', '')+'.service')
    if not group.exists(): return {'cgroup_inode': None, 'processes': []}
    found = []
    for pid in (group/'cgroup.procs').read_text().split():
        p = Path('/proc')/pid
        try:
            exe = str(p.joinpath('exe').resolve(strict=True))
            if 'qemu-system-' not in Path(exe).name: continue
            args = p.joinpath('cmdline').read_bytes().split(b'\0')
            names = [args[i+1].decode().split(',')[0] for i in range(len(args)-1) if args[i] == b'-name']
            assert 'hc-'+vid.replace('-', '') in names
            fields = p.joinpath('stat').read_text().rsplit(') ', 1)[1].split()
            found.append({'pid': int(pid), 'ppid': int(fields[1]), 'start_ticks': int(fields[19]), 'state': fields[0], 'exe': exe})
        except (OSError, ValueError): continue
    return {'cgroup_inode': group.stat().st_ino, 'processes': found}


def sample(owner, prefix):
    result = {}
    with closing(sqlite3.connect('file:/var/lib/homecloud/control/state.db?mode=ro', uri=True, timeout=2)) as db:
        rows = db.execute("SELECT record FROM vms WHERE json_extract(record,'$.spec.name') LIKE ?", [prefix+'-%']).fetchall()
        for row in rows:
            vm = json.loads(row[0]); vid = vm['id']
            assert vm['owner'] == owner and not vm['spec'].get('applications')
            assert re.fullmatch(r'[a-f0-9-]{36}', vid)
            if vm['observed'] == 'deleted': continue
            name = 'hc-'+vid.replace('-', '')
            item = {'controller_desired': vm['desired'], 'controller_observed': vm['observed'], 'controller_error': vm.get('error'),
                    'operations': [list(x) for x in db.execute("SELECT id, phase FROM operations WHERE vm_id=? AND phase NOT IN ('succeeded','failed')", [vid])],
                    **processes(vid)}
            state = api('/1.0/instances/'+name+'/state')
            if state is not None:
                item['incus_state'] = state['status']; item['incus_pid'] = state.get('pid')
                instance = api('/1.0/instances/'+name)
                if instance is not None:
                    config = instance['config']
                    assert config['user.homecloud.id'] == vid and config['user.homecloud.owner'] == owner
                    item['recorded_cid'] = config.get('volatile.vsock_id')
                conf = Path('/run/incus')/name/'qemu.conf'
                try:
                    match = re.search(r'guest-cid\s*=\s*"(\d+)"', conf.read_text())
                    item['generated_cid'] = match.group(1) if match else None
                except FileNotFoundError: pass
            for kind in ['launch', 'startup', 'setup']:
                path = Path('/var/lib/homecloud/host')/(kind+'-'+vid.replace('-', '')+'.json')
                try: item[kind] = json.loads(path.read_text())
                except FileNotFoundError: pass
            result[vid] = item
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--owner', required=True); p.add_argument('--prefix', required=True)
    p.add_argument('--output', required=True); p.add_argument('--seconds', type=int, default=5400)
    a = p.parse_args()
    assert os.geteuid() == 0
    assert re.fullmatch(r'hc-proof-[a-f0-9]{10}', a.prefix)
    assert re.fullmatch(r'[a-f0-9-]{36}', a.owner)
    assert 1 <= a.seconds <= 6000
    deadline = time.monotonic()+a.seconds; previous = None; last = 0
    stopping = [False]
    def stop(*_): stopping[0] = True
    signal.signal(signal.SIGINT, stop); signal.signal(signal.SIGTERM, stop)
    with Path(a.output).open('x') as out:
        os.chmod(a.output, 0o600)
        while time.monotonic() < deadline and not stopping[0]:
            if Path(a.output).stat().st_size >= 32*1024**2: raise RuntimeError('Witness size bound reached')
            now = time.monotonic()
            try: value = sample(a.owner, a.prefix)
            except Exception as error: value = {'sample_error': type(error).__name__+': '+str(error)}
            if value != previous or now-last >= 5:
                out.write(json.dumps({'ns': time.monotonic_ns(), 'unix_ns': time.time_ns(), 'vms': value})+'\n'); out.flush()
                previous = value; last = now
            time.sleep(max(0.01, 0.25-(time.monotonic()-now)))


if __name__ == '__main__': main()
