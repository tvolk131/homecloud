#!/usr/bin/python3
"""Local-root software updates. Never edits host policy or restarts Incus/QEMU.

The durable journal commits a matched file set before the new daemon can run.
A systemd pre-start guard restores an interrupted, uncommitted set. After
activation, rollback restores software only: acknowledged database work survives.
"""
import argparse
import contextlib
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid

FORMAT = 1
STATE_FORMAT = 1  # Bump for incompatible persisted state, not just SQL DDL changes.
GUARD = '/usr/local/libexec/homecloud-deploy.py'
HELPER = '/usr/local/libexec/homecloud/'
HOST_FILES = ('host_runtime.py','managed_apps.py','managed_host.py','memory_capacity.py',
              'resource_reserves.py','arc_policy.py','instance_runtime.py')
GUEST_FILES = ('managed-host.py',)
# The guest runner is pushed into VMs, which may run an older Ubuntu than the
# host, so it must be a static executable.
GUEST_RUNNER = 'bin/homecloud-guest'
PAYLOAD = {'bin/homecloudd':'/usr/local/bin/homecloudd','bin/homecloud':'/usr/local/bin/homecloud',
           GUEST_RUNNER:HELPER+'homecloud-guest',
           **{'crates/host/'+n:HELPER+n for n in HOST_FILES},
           **{'guest/'+n:HELPER+n for n in GUEST_FILES}}
BOUNDARY = ('crates/host/native/qemu_cpu_launcher.c','crates/host/firewall.nft','scripts/host_deploy.py')
SOURCES = set(PAYLOAD) | set(BOUNDARY) | {'scripts/install_host.py'}
MAX_FILE = 128 * 1024**2
MAX_TOTAL = 300 * 1024**2


def require(ok, message):
    if not ok: raise RuntimeError(message)


def digest(data): return hashlib.sha256(data).hexdigest()


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try: os.fsync(fd)
    finally: os.close(fd)


def atomic(path, data, mode=0o600):
    path = Path(path); path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.'+path.name+'-', dir=path.parent)
    try:
        with os.fdopen(fd,'wb') as f:
            os.fchmod(f.fileno(),mode); f.write(data); f.flush(); os.fsync(f.fileno())
        os.replace(name,path); sync_dir(path.parent)
    finally:
        if os.path.exists(name): os.unlink(name)


def save(path, value): atomic(path, (json.dumps(value,sort_keys=True,indent=2)+'\n').encode())


def clear_cache(path):
    if path.suffix=='.py':
        for cached in (path.parent/'__pycache__').glob(path.stem+'.*.pyc'):
            require(cached.is_file() and not cached.is_symlink(),'Unexpected Python cache path')
            cached.unlink()


def static_elf(data):
    """True for a Linux x86-64 executable with no program interpreter."""
    if not (len(data)>64 and data[:5]==b'\x7fELF\x02' and data[5]==1 and data[18:20]==b'>\x00'):return False
    offset=int.from_bytes(data[32:40],'little');size=int.from_bytes(data[54:56],'little');count=int.from_bytes(data[56:58],'little')
    if size<4 or offset+size*count>len(data):return False
    return all(int.from_bytes(data[offset+i*size:offset+i*size+4],'little')!=3 for i in range(count))


def read_bundle(path, expected):
    require(re.fullmatch('[0-9a-f]{64}',expected), 'Supply the reviewed bundle SHA-256')
    # Copy verified bytes to a private staging area, never reopen a user path later.
    with Path(path).open('rb') as f: raw = f.read(MAX_TOTAL+1)
    require(len(raw)<=MAX_TOTAL and digest(raw)==expected, 'Bundle size or SHA-256 mismatch')
    files = {}; total=0
    with tarfile.open(fileobj=io.BytesIO(raw),mode='r:gz') as archive:
        for entry in archive:
            name=entry.name; p=PurePosixPath(name)
            require(entry.isfile() and name==str(p) and not p.is_absolute() and '..' not in p.parts,
                    'Bundle must contain canonical regular files only')
            require(name in SOURCES or name=='manifest.json','Unexpected bundle member: '+name)
            require(name not in files and 0<=entry.size<=MAX_FILE,'Duplicate or oversized bundle member')
            total+=entry.size;require(total<=MAX_TOTAL,'Expanded bundle exceeds size limit')
            files[name]=archive.extractfile(entry).read()
    require(set(files)==SOURCES|{'manifest.json'},'Bundle is incomplete')
    manifest=json.loads(files.pop('manifest.json'))
    require(manifest.get('format')==FORMAT and manifest.get('state_format')==STATE_FORMAT,
            'Unsupported bundle/state format; needs an explicit migration procedure')
    require(manifest.get('architecture')=='x86_64','Unsupported bundle architecture')
    require(set(manifest['files'])==SOURCES,'Manifest file set differs')
    for name,data in files.items():
        require(manifest['files'][name]=={'sha256':digest(data),'bytes':len(data)},'Bundle content mismatch: '+name)
        if name.endswith('.py'): compile(data,name,'exec')
        if name.startswith('bin/'):
            require(len(data)>20 and data[:5]==b'\x7fELF\x02' and data[5]==1 and data[18:20]==b'>\x00',
                    'Expected a Linux x86-64 executable: '+name)
    require(static_elf(files[GUEST_RUNNER]),'Expected a static guest runner: '+GUEST_RUNNER)
    return manifest,files


class Services:
    def run(self,*args,timeout=60):
        p=subprocess.run(args,capture_output=True,text=True,timeout=timeout)
        require(p.returncode==0, args[0]+': '+(p.stderr or p.stdout)[-1000:])
        return p.stdout.strip()
    def stop(self): self.run('systemctl','stop','homecloudd.service')
    def active(self):
        return subprocess.run(['systemctl','is-active','--quiet','homecloudd'],capture_output=True).returncode==0
    def start(self): self.run('systemctl','start','homecloudd.service')
    def check(self,files):
        require(platform.system()=='Linux' and platform.machine()=='x86_64','Requires Linux x86-64')
        for command in ('systemctl','incus','zfs','zpool','nft','gcc','visudo'):
            require(shutil.which(command),'Missing prerequisite: '+command)
        os_release=Path('/etc/os-release').read_text()
        require('ID=ubuntu\n' in os_release and 'VERSION_ID="26.04"' in os_release,
                'This deployment path is validated on Ubuntu 26.04; review and test other platforms explicitly')
        versions={'incus':self.run('incus','version'),'zfs':self.run('zfs','version'),
                  'qemu':self.run('/usr/bin/qemu-system-x86_64','--version')}
        require(versions['incus'].splitlines()[0].endswith('6.0.5') and
                versions['zfs'].splitlines()[0].startswith('zfs-2.4.1') and
                'version 10.2.1' in versions['qemu'].splitlines()[0],
                'Untested Incus/ZFS/QEMU versions; validate a maintenance migration first')
        self.run('systemctl','is-active','incus.service')
        with tempfile.TemporaryDirectory(prefix='homecloud-preflight-') as temporary:
            for name in ('homecloudd','homecloud','homecloud-guest'):
                candidate=Path(temporary)/name;candidate.write_bytes(files['bin/'+name]);candidate.chmod(0o700)
                self.run(str(candidate),'--help',timeout=15)
        return {k:v.splitlines()[0] for k,v in versions.items()}
    def ready(self,layout,sha,started):
        deadline=time.monotonic()+40
        while time.monotonic()<deadline:
            if self.active():
                try:
                    pid=int(self.run('systemctl','show','homecloudd','--property=MainPID','--value'))
                    ticket=layout.path('/var/lib/homecloud/control/server.json')
                    if pid>0 and digest(Path(f'/proc/{pid}/exe').read_bytes())==sha and ticket.stat().st_mtime_ns>=started:
                        return
                except (OSError,RuntimeError,ValueError): pass
            time.sleep(.25)
        raise RuntimeError('New controller did not become ready. Files are committed; inspect logs or explicitly roll back software. Database was retained.')


class Deployment:
    def __init__(self,root=Path('/'),services=None):
        self.root=Path(root);self.services=services or Services()
        self.state=self.path('/var/lib/homecloud/deployments')
        self.current=self.state/'installed.json';self.journal=self.state/'transaction.json'
    def path(self,p): return self.root / p.lstrip('/')
    @contextlib.contextmanager
    def lock(self,nonblocking=False):
        self.state.mkdir(parents=True,exist_ok=True,mode=0o700);self.state.chmod(0o700)
        with (self.state/'deployment.lock').open('a') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX | (fcntl.LOCK_NB if nonblocking else 0));yield
    @contextlib.contextmanager
    def host_lock(self):
        p=self.path('/var/lib/homecloud/host/operations.lock')
        require(p.parent.is_dir(),'Host state is missing')
        with p.open('a') as lock:
            deadline=time.monotonic()+120
            while True:
                try:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB);break
                except BlockingIOError:
                    require(time.monotonic()<deadline,'A host operation is still active; no files were changed')
                    time.sleep(.1)
            yield
    def adopt_legacy(self,source,expected):
        # Explicit local-root transition from pre-receipt installations. It does
        # not silently declare arbitrary files compatible with a new release.
        source=Path(source).resolve()
        require(re.fullmatch('[0-9a-f]{64}',expected),'Supply the reviewed installed daemon SHA-256')
        with self.lock():
            require(not self.current.exists(),'Installation already has a receipt')
            require(digest(self.path('/usr/local/bin/homecloudd').read_bytes())==expected,'Installed daemon differs from the reviewed legacy binary')
            for name,destination in PAYLOAD.items():
                require(self.path(destination).is_file(),'Missing legacy file: '+destination)
                if not name.startswith('bin/'):
                    require(digest(self.path(destination).read_bytes())==digest((source/name).read_bytes()),'Legacy source does not match installed helper: '+name)
            require(self.path('/etc/homecloud/firewall.nft').read_bytes()==(source/'crates/host/firewall.nft').read_bytes(),'Legacy firewall differs')
            require((source/'scripts/host_deploy.py').read_bytes()==Path(__file__).read_bytes(),'Run the deployment engine from its reviewed source tree')
            with tempfile.TemporaryDirectory(prefix='homecloud-native-check-') as temporary:
                native=Path(temporary)/'qemu';self.services.run('gcc','-O2','-Wall','-Wextra','-Werror','-static','-o',str(native),str(source/'crates/host/native/qemu_cpu_launcher.c'))
                require(digest(native.read_bytes())==digest(self.path(HELPER+'qemu/qemu-system-x86_64').read_bytes()),'Legacy native launcher differs; explicit maintenance required')
            require(self.services.active(),'Legacy controller must be healthy before adoption')
            self.services.stop()
            with self.host_lock():
                atomic(self.path(GUARD),(source/'scripts/host_deploy.py').read_bytes(),0o755)
                for unit in ('homecloudd.service','homecloud-host-layout.service'):
                    drop=self.path('/etc/systemd/system/'+unit+'.d/homecloud-deployment.conf')
                    atomic(drop,('[Service]\nExecStartPre=+/usr/bin/python3 '+GUARD+' guard\n').encode(),0o644)
                r=self.register(source)
            self.services.run('systemctl','daemon-reload')
        self.services.start()
        return {'status':'adopted','installation':r['id'],'restarts':['homecloudd.service']}
    def receipt(self):
        require(self.current.is_file(),'No installation receipt. Run the explicit legacy adoption or a clean install first.')
        r=json.loads(self.current.read_text())
        require(r['format']==FORMAT and r['state_format']==STATE_FORMAT,'Installed state format is incompatible')
        return r
    def verify(self,receipt):
        require(set(receipt['files'])==set(PAYLOAD.values())|{GUARD},'Invalid installed file set')
        for name,sha in receipt['files'].items():
            p=self.path(name);require(p.is_file() and not p.is_symlink() and digest(p.read_bytes())==sha,
                                     'Installed software drift: '+name)
    def preflight(self,manifest,files):
        r=self.receipt();self.verify(r)
        require(r['boundary']=={n:digest(files[n]) for n in BOUNDARY},
                'Native launcher/firewall/deployment-engine changes require an explicit maintenance installation')
        require(self.services.active(),'Controller must be healthy before a normal upgrade')
        require(not self.journal.exists() or json.loads(self.journal.read_text())['phase'] in ('committed','rolled_back'),
                'An interrupted deployment needs the pre-start guard before another update')
        policy=json.loads(self.path('/etc/homecloud/host.json').read_text())
        require(self.path('/etc/homecloud/control.json').is_file(),'Controller configuration is missing')
        require('is healthy' in self.services.run('zpool','status','-x',policy['pool']), 'Pool needs operator review')
        require(shutil.disk_usage(self.state).free>2*sum(len(d) for d in files.values())+256*1024**2,
                'Insufficient staging and recovery space')
        for unit in ('homecloudd.service','homecloud-host-layout.service'):
            require(GUARD+' guard' in self.services.run('systemctl','cat',unit), 'Missing deployment guard: '+unit)
        return self.services.check(files)
    def register(self,source):
        """Called only by explicit install/adoption, while software is quiescent."""
        files={p:digest(self.path(p).read_bytes()) for p in (*PAYLOAD.values(),GUARD)}
        r={'format':FORMAT,'state_format':STATE_FORMAT,'id':str(uuid.uuid4()),'files':files,
           'boundary':{n:digest((Path(source)/n).read_bytes()) for n in BOUNDARY}}
        save(self.current,r);return r
    def recover(self):
        if not self.journal.exists():return
        j=json.loads(self.journal.read_text())
        if j['phase'] in ('committed','rolled_back'):return
        require(j['phase']=='applying','Unknown deployment phase; inspect before starting')
        directory=self.state/j['id'];require(str(uuid.UUID(j['id']))==j['id'],'Invalid deployment ID')
        before=json.loads((directory/'before.json').read_text())
        for name,entry in before['files'].items():
            require(name in set(PAYLOAD.values())|{GUARD},'Invalid recovery path')
            data=(directory/'before'/name.lstrip('/')).read_bytes();require(digest(data)==entry['sha256'],'Recovery copy was modified')
            atomic(self.path(name),data,entry['mode']);clear_cache(self.path(name))
        save(self.current,before['receipt']);j['phase']='rolled_back';save(self.journal,j)
        self.verify(before['receipt'])
    def guard(self):
        # Refuse a concurrent writer. An updater never starts services until it
        # has committed and released this lock. Kernel locks survive PID reuse.
        with self.lock(nonblocking=True):
            with self.host_lock():self.recover();self.verify(self.receipt())
    def checkpoint(self,directory,old):
        before={'receipt':old,'files':{}}
        for name in old['files']:
            p=self.path(name);data=p.read_bytes();entry={'sha256':digest(data),'mode':p.stat().st_mode&0o777}
            atomic(directory/'before'/name.lstrip('/'),data,entry['mode']);before['files'][name]=entry
        save(directory/'before.json',before)
        # Evidence only. Never restore this over acknowledged application work.
        database=self.path('/var/lib/homecloud/control/state.db')
        if database.exists():
            with sqlite3.connect('file:'+str(database)+'?mode=ro',uri=True) as src,sqlite3.connect(directory/'state-before.db') as dst:
                src.backup(dst)
            (directory/'state-before.db').chmod(0o600)
        return before
    def update(self,manifest,files):
        started=None;error=None;was_stopped=False
        with self.lock():
            self.preflight(manifest,files);old=self.receipt();ident=str(uuid.uuid4());directory=self.state/ident;directory.mkdir(mode=0o700)
            # Stop just the controller, then wait for any surviving root helper.
            # The exact host operation lock also fences native lifecycle effects.
            self.services.stop();was_stopped=True
            try:
                with self.host_lock():
                    self.verify(old);self.checkpoint(directory,old)
                    j={'format':FORMAT,'id':ident,'phase':'applying'};save(self.journal,j)
                    for name,destination in PAYLOAD.items():
                        atomic(self.path(destination),files[name],0o755 if name.startswith('bin/') or name.endswith('host_runtime.py') else 0o644);clear_cache(self.path(destination))
                    new={'format':FORMAT,'state_format':STATE_FORMAT,'id':ident,'files':{p:digest(self.path(p).read_bytes()) for p in old['files']},'boundary':old['boundary']}
                    self.verify(new);save(self.current,new)
                    j['phase']='committed';save(self.journal,j);started=time.time_ns()
            except BaseException as e:
                error=e
                # Restore only before activation. An uncatchable kill is handled
                # by the independent systemd guard on the next service start.
                with self.host_lock():self.recover()
        if was_stopped:self.services.start()
        if error:raise error
        self.services.ready(self,digest(files['bin/homecloudd']),started)
        return {'deployment':ident,'status':'ready','restarts':['homecloudd.service']}
    def rollback(self,ident):
        require(str(uuid.UUID(ident))==ident,'Invalid deployment ID')
        with self.lock():
            old=self.receipt();require(old['id']==ident,'Rollback is only available for the currently installed deployment')
            directory=self.state/ident;before=json.loads((directory/'before.json').read_text())
            require(before['receipt']['state_format']==old['state_format'],'Incompatible persisted state; explicit recovery required')
            self.verify(old);self.services.stop()
            with self.host_lock():
                # Durable applying journal lets the same guard resume rollback.
                save(self.journal,{'format':FORMAT,'id':ident,'phase':'applying'});self.recover();started=time.time_ns()
        self.services.start();self.services.ready(self,before['receipt']['files']['/usr/local/bin/homecloudd'],started)
        return {'deployment':ident,'status':'rolled_back','database':'retained'}


def main():
    p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='command',required=True)
    for name in ('check','upgrade'):
        cmd=sub.add_parser(name);cmd.add_argument('--bundle',type=Path,required=True);cmd.add_argument('--sha256',required=True)
    sub.add_parser('guard');cmd=sub.add_parser('rollback');cmd.add_argument('deployment')
    cmd=sub.add_parser('adopt-legacy');cmd.add_argument('--source',required=True,type=Path);cmd.add_argument('--expected-daemon-sha256',required=True)
    args=p.parse_args();require(os.geteuid()==0,'Run locally with sudo');d=Deployment()
    if args.command=='guard':d.guard();return
    if args.command=='adopt-legacy':result=d.adopt_legacy(args.source,args.expected_daemon_sha256)
    elif args.command=='rollback':result=d.rollback(args.deployment)
    else:
        manifest,files=read_bundle(args.bundle,args.sha256)
        if args.command=='check':
            with d.lock():result={'status':'checked','versions':d.preflight(manifest,files)}
        else:result=d.update(manifest,files)
    print(json.dumps(result))


if __name__=='__main__':
    try:main()
    except Exception as e:print('Homecloud deployment: '+str(e),file=sys.stderr);sys.exit(1)
