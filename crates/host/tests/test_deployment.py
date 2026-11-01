"""Deployment boundary faults with real files, SQLite and kernel locks."""
import importlib.util
import io
import json
from pathlib import Path
import sqlite3
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SOURCE=Path(__file__).resolve().parents[3]/'scripts/host_deploy.py'
spec=importlib.util.spec_from_file_location('homecloud_deploy',SOURCE)
d=importlib.util.module_from_spec(spec);spec.loader.exec_module(d)
ELF=b'\x7fELF\x02\x01'+bytes(12)+b'>\x00'+b'fixture'


def files(version):
    return {name:(ELF+version.encode() if name.startswith('bin/') else ('# '+name+' '+version+'\n').encode()) for name in d.SOURCES}


def bundle(path,content,extra=(),manifest_edit=None):
    manifest={'format':d.FORMAT,'state_format':d.STATE_FORMAT,'architecture':'x86_64','files':{n:{'sha256':d.digest(v),'bytes':len(v)} for n,v in content.items()}}
    if manifest_edit:manifest_edit(manifest)
    with tarfile.open(path,'w:gz') as archive:
        for name,data in list(content.items())+[('manifest.json',json.dumps(manifest).encode())]+list(extra):
            entry=tarfile.TarInfo(name);entry.size=len(data);archive.addfile(entry,io.BytesIO(data))
    return d.digest(path.read_bytes())


class Services:
    def __init__(self):self.calls=[];self.on=True;self.fail_ready=False;self.deployment=None
    def run(self,*args,**kwargs):
        self.calls.append(args)
        if args[0]=='zpool':return "pool 'test' is healthy"
        return d.GUARD+' guard'
    def check(self,files):return {'fixture':'validated'}
    def stop(self):self.calls.append(('stop','homecloudd'));self.on=False
    def active(self):return self.on
    def start(self):
        # Runs the independent systemd guard, including its nonblocking lock.
        self.deployment.guard();self.on=True;self.calls.append(('start','homecloudd'))
    def ready(self,layout,sha,started):
        if self.fail_ready:raise RuntimeError('startup failed')
        self.assert_sha=sha


class Deployments(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name)
        self.services=Services();self.d=d.Deployment(self.root,self.services);self.services.deployment=self.d
        self.old=files('old');self.new=files('new')
        for name in d.BOUNDARY:self.new[name]=self.old[name]
        for name,destination in d.PAYLOAD.items():d.atomic(self.d.path(destination),self.old[name],0o755 if name.startswith('bin/') else 0o644)
        d.atomic(self.d.path(d.GUARD),self.old['scripts/host_deploy.py'],0o755)
        source=self.root/'source'
        for name in d.BOUNDARY:d.atomic(source/name,self.old[name])
        self.d.path('/var/lib/homecloud/host').mkdir(parents=True)
        d.save(self.d.path('/etc/homecloud/host.json'),{'pool':'test'})
        d.save(self.d.path('/etc/homecloud/control.json'),{'unchanged':True})
        self.db=self.d.path('/var/lib/homecloud/control/state.db');self.db.parent.mkdir(parents=True)
        with sqlite3.connect(self.db) as db:db.execute('CREATE TABLE intents (id)');db.execute('INSERT INTO intents VALUES (1)')
        with self.d.lock():self.receipt=self.d.register(source)
    def assert_old(self):
        for name,destination in d.PAYLOAD.items():self.assertEqual(self.d.path(destination).read_bytes(),self.old[name])
        self.d.verify(self.d.receipt())
    def test_matched_upgrade_and_software_rollback_keep_acknowledged_work(self):
        original_policy=self.d.path('/etc/homecloud/host.json').read_bytes()
        result=self.d.update({},self.new)
        for name,p in d.PAYLOAD.items():self.assertEqual(self.d.path(p).read_bytes(),self.new[name])
        with sqlite3.connect(self.db) as db:db.execute('INSERT INTO intents VALUES (2)')
        self.d.rollback(result['deployment']);self.assert_old()
        with sqlite3.connect(self.db) as db:self.assertEqual(db.execute('SELECT id FROM intents').fetchall(),[(1,),(2,)])
        self.assertEqual(self.d.path('/etc/homecloud/host.json').read_bytes(),original_policy)
        self.assertFalse(any('incus' in call for call in self.services.calls))
    def test_startup_failure_is_explicit_and_does_not_restore_database(self):
        self.services.fail_ready=True
        with self.assertRaisesRegex(RuntimeError,'startup failed'):self.d.update({},self.new)
        self.assertEqual(json.loads(self.d.journal.read_text())['phase'],'committed')
        self.assertEqual(self.d.path('/usr/local/bin/homecloudd').read_bytes(),self.new['bin/homecloudd'])
        self.services.fail_ready=False;self.d.rollback(self.d.receipt()['id']);self.assert_old()
    def test_every_payload_write_failure_restores_complete_before_set(self):
        real=d.atomic
        for destination in d.PAYLOAD.values():
            fired=[]
            def fail(path,data,mode=0o600):
                if Path(path)==self.d.path(destination) and not fired:
                    fired.append(True);raise OSError('injected disk failure')
                return real(path,data,mode)
            with patch.object(d,'atomic',fail):
                with self.assertRaisesRegex(OSError,'injected disk failure'):self.d.update({},self.new)
            self.assert_old();self.assertTrue(self.services.on)
    def test_prestart_guard_restores_an_uncatchable_interrupted_upgrade(self):
        ident=str(d.uuid.uuid4());directory=self.d.state/ident;directory.mkdir()
        self.d.checkpoint(directory,self.receipt)
        d.save(self.d.journal,{'id':ident,'phase':'applying'})
        d.atomic(self.d.path('/usr/local/bin/homecloudd'),self.new['bin/homecloudd'],0o755)
        # Models a killed updater: Python cleanup never runs.
        self.d.guard();self.assert_old()
        self.assertEqual(json.loads(self.d.journal.read_text())['phase'],'rolled_back')
    def test_guard_refuses_a_live_writer_without_restoring_it(self):
        with self.d.lock():
            with self.assertRaises(BlockingIOError):self.d.guard()
        self.assert_old()
    def test_policy_boundary_and_installed_drift_reject_before_stopping(self):
        bad=dict(self.new);bad['crates/host/firewall.nft']=b'different'
        with self.assertRaisesRegex(RuntimeError,'maintenance'):self.d.update({},bad)
        self.assertFalse(any(c[0]=='stop' for c in self.services.calls))
        d.atomic(self.d.path('/usr/local/bin/homecloudd'),b'foreign')
        with self.assertRaisesRegex(RuntimeError,'drift'):self.d.update({},self.new)
        self.assertFalse(any(c[0]=='stop' for c in self.services.calls))
    def test_changed_python_cache_is_removed_on_update(self):
        source=self.d.path(d.HELPER+'arc_policy.py');cache=source.parent/'__pycache__/arc_policy.cpython-314.pyc';d.atomic(cache,b'stale')
        self.d.update({},self.new);self.assertFalse(cache.exists())
    def test_corrupt_recovery_copy_blocks_service_start(self):
        ident=str(d.uuid.uuid4());directory=self.d.state/ident;directory.mkdir();self.d.checkpoint(directory,self.receipt)
        d.save(self.d.journal,{'id':ident,'phase':'applying'})
        d.atomic(directory/'before/usr/local/bin/homecloudd',b'corrupt')
        with self.assertRaisesRegex(RuntimeError,'modified'):self.d.guard()
    def test_format_mismatch_never_stops_controller(self):
        record=self.d.receipt();record['state_format']+=1;d.save(self.d.current,record)
        with self.assertRaisesRegex(RuntimeError,'incompatible'):self.d.update({},self.new)
        self.assertFalse(any(c[0]=='stop' for c in self.services.calls))

    def test_observer_updates_and_rolls_back_with_daemon_and_missing_binary_blocks_start(self):
        path=d.HELPER+'homecloud-host-observer'
        result=self.d.update({},self.new)
        self.assertEqual(self.d.path(path).read_bytes(),self.new['bin/homecloud-host-observer'])
        self.assertEqual(self.d.path(path).stat().st_mode&0o777,0o755)
        self.d.rollback(result['deployment'])
        self.assertEqual(self.d.path(path).read_bytes(),self.old['bin/homecloud-host-observer'])
        self.assertEqual(self.d.path(path).stat().st_mode&0o777,0o755)
        self.d.path(path).unlink()
        with self.assertRaisesRegex(RuntimeError,'drift'):self.d.guard()

    def test_pre_cutover_receipt_is_refused_before_service_changes(self):
        record=self.d.receipt();record['files'].pop(d.HELPER+'homecloud-host-observer');d.save(self.d.current,record)
        with self.assertRaisesRegex(RuntimeError,'maintenance installation'):self.d.update({},self.new)
        self.assertFalse(any(c[0]=='stop' for c in self.services.calls))


class BundleValidation(unittest.TestCase):
    def setUp(self):self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.path=Path(self.temp.name)/'bundle.tgz'
    def test_missing_or_invalid_native_observer_is_rejected(self):
        content=files('valid');content.pop('bin/homecloud-host-observer')
        sha=bundle(self.path,content)
        with self.assertRaisesRegex(RuntimeError,'incomplete'):d.read_bundle(self.path,sha)
        content=files('valid');content['bin/homecloud-host-observer']=b'#!/bin/sh\nexit 0\n'
        sha=bundle(self.path,content)
        with self.assertRaisesRegex(RuntimeError,'Linux x86-64 executable'):d.read_bundle(self.path,sha)
    def test_valid_and_corrupted_digest(self):
        content=files('valid');sha=bundle(self.path,content)
        self.assertEqual(d.read_bundle(self.path,sha)[1],content)
        with self.assertRaisesRegex(RuntimeError,'mismatch'):d.read_bundle(self.path,'0'*64)
    def test_traversal_duplicates_unknown_files_and_incomplete_manifest(self):
        for extra in [[('../escape',b'x')],[('/escape',b'x')],[('bin/homecloud',b'x')],[('guest/unreviewed.sh',b'x')]]:
            sha=bundle(self.path,files('bad'),extra)
            with self.assertRaises(RuntimeError):d.read_bundle(self.path,sha)
        sha=bundle(self.path,files('bad'),manifest_edit=lambda m:m['files'].pop('bin/homecloud'))
        with self.assertRaises(RuntimeError):d.read_bundle(self.path,sha)
    def test_state_format_architecture_content_and_syntax_must_match(self):
        for field,value in [('state_format',2),('architecture','aarch64')]:
            sha=bundle(self.path,files('bad'),manifest_edit=lambda m:m.update({field:value}))
            with self.assertRaises(RuntimeError):d.read_bundle(self.path,sha)
        content=files('bad');content['guest/managed-app.py']=b'def broken(\n'
        sha=bundle(self.path,content)
        with self.assertRaises(SyntaxError):d.read_bundle(self.path,sha)
        sha=bundle(self.path,files('bad'),manifest_edit=lambda m:m['files']['bin/homecloud'].update(sha256='0'*64))
        with self.assertRaisesRegex(RuntimeError,'content mismatch'):d.read_bundle(self.path,sha)


class InstallPreflight(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name)
        spec=importlib.util.spec_from_file_location('installation_preflight',SOURCE.with_name('install_host.py'))
        self.install=importlib.util.module_from_spec(spec)
        with patch.dict(sys.modules,{'host_deploy':d}):spec.loader.exec_module(self.install)
        self.policy=self.root/'policy.json'
        self.policy.write_text(json.dumps({'pool':'test','dataset_root':'test/homecloud','image':'a'*64,
            'cpu_groups':[[2,3]],'housekeeping':[0,1],
            'capacity':{'memory_bytes':4*1024**3,'cpu_cores':1,'disk_bytes':20*1024**3}}))
        for name in ('homecloudd','homecloud','homecloud-host-observer'):(self.root/name).write_bytes(ELF)

    def test_missing_observer_fails_before_installation_or_preflight_commands(self):
        (self.root/'homecloud-host-observer').unlink()
        with patch.object(d.os,'geteuid',return_value=0),patch.object(d.shutil,'which',return_value='/fixture'), \
             patch.object(sys,'argv',['install_host.py','--check','--policy',str(self.policy),'--daemon',str(self.root/'homecloudd')]), \
             patch.object(self.install,'run') as run,patch.object(self.install,'write') as write:
            with self.assertRaisesRegex(AssertionError,'matching Rust observer'):self.install.main()
            run.assert_not_called();write.assert_not_called()

    def test_check_requires_all_three_binaries_and_does_not_install(self):
        real_read=Path.read_text;real_exists=Path.exists
        def read(path,*args,**kwargs):
            if str(path).startswith('/sys/devices/system/cpu/'):return '2-3'
            return real_read(path,*args,**kwargs)
        def exists(path):return str(path).startswith('/sys/devices/system/cpu/') or real_exists(path)
        with patch.object(d.os,'geteuid',return_value=0),patch.object(d.shutil,'which',return_value='/fixture'), \
             patch.object(d.os,'sysconf',side_effect=lambda key:2*1024**2 if key=='SC_PHYS_PAGES' else 4096), \
             patch.object(Path,'read_text',autospec=True,side_effect=read),patch.object(Path,'exists',autospec=True,side_effect=exists), \
             patch.object(d.subprocess,'check_output',return_value="pool 'test' is healthy"), \
             patch.object(d.Services,'check',return_value={}) as check, \
             patch.object(sys,'argv',['install_host.py','--check','--policy',str(self.policy),'--daemon',str(self.root/'homecloudd')]), \
             patch.object(self.install,'run'),patch.object(self.install,'write') as write:
            self.install.main();write.assert_not_called()
            self.assertEqual(set(check.call_args.args[0]),{'bin/homecloudd','bin/homecloud','bin/homecloud-host-observer'})


class ObserverBinaryPreflight(unittest.TestCase):
    def test_observer_help_must_succeed_before_activation(self):
        calls=[]
        def run(*args,**kwargs):
            calls.append(args)
            if args[0]=='incus':return 'Client version: 6.0.5'
            if args[0]=='zfs':return 'zfs-2.4.1'
            if args[0]=='/usr/bin/qemu-system-x86_64':return 'QEMU emulator version 10.2.1'
            if Path(args[0]).name=='homecloud-host-observer':raise RuntimeError('observer cannot start')
            return 'active'
        with patch.object(d.platform,'system',return_value='Linux'),patch.object(d.platform,'machine',return_value='x86_64'), \
             patch.object(d.shutil,'which',return_value='/fixture'),patch.object(d.os,'access',return_value=True), \
             patch.object(Path,'is_file',return_value=True), \
             patch.object(Path,'read_text',return_value='ID=ubuntu\nVERSION_ID="26.04"\n'),patch.object(d.Services,'run',side_effect=run):
            with self.assertRaisesRegex(RuntimeError,'observer cannot start'):d.Services().check(files('candidate'))
        self.assertTrue(any(Path(call[0]).name=='homecloud-host-observer' and call[1:] == ('--help',) for call in calls))
        self.assertFalse(any('stop' in call or 'start' in call for call in calls))

if __name__=='__main__':unittest.main()
