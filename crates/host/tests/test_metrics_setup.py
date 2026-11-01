import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPTS=Path(__file__).resolve().parents[3]/'scripts'
sys.path.insert(0,str(SCRIPTS))
spec=importlib.util.spec_from_file_location('homecloud_install',SCRIPTS/'install_host.py')
installer=importlib.util.module_from_spec(spec);spec.loader.exec_module(installer)


class MetricsSetup(unittest.TestCase):
    def setUp(self):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        self.directory=Path(temporary.name)
        self.control=self.directory/'control.json'
        self.control.write_text(json.dumps({'limits':{'capacity':{}},'other_setting':'preserved'}))
        self.original=self.control.read_bytes()
        self.settings={}
        self.trusted=[];self.commands=[];self.generation=0;self.fail_probe=False;self.expired=False
        for target,replacement in [('incus_query',self.query),('run',self.command)]:
            mocked=patch.object(installer,target,replacement);mocked.start();self.addCleanup(mocked.stop)
        for target,replacement in [('subprocess.check_output',self.output),('subprocess.run',self.process),
                                   ('shutil.which',lambda program:'/usr/bin/'+program),
                                   ('pwd.getpwnam',lambda name:SimpleNamespace(pw_gid=os.getgid())),
                                   ('os.chown',lambda *args:None),('os.fchown',lambda *args:None)]:
            module,attribute=target.split('.')
            mocked=patch.object(getattr(installer,module),attribute,replacement)
            mocked.start();self.addCleanup(mocked.stop)

    def query(self,path):
        if path=='/1.0':return {'config':self.settings,'environment':{'certificate':'SERVER CERTIFICATE'}}
        self.assertEqual(path,'/1.0/certificates?recursion=1')
        return self.trusted

    def output(self,args,**kwargs):
        if '-ext' in args:return b'X509v3 Subject Alternative Name:\n DNS:incus.local, DNS:resolvr-box\n'
        identity=Path(args[args.index('-in')+1]).read_bytes().split(b'KEY:')[0]
        return b'DER:'+identity if args[-1]=='DER' else identity

    def process(self,args,**kwargs):
        if args[:3]==['openssl','req','-x509']:
            self.generation+=1
            Path(args[args.index('-out')+1]).write_bytes(('CERT:'+str(self.generation)+'\n').encode())
            Path(args[args.index('-keyout')+1]).write_bytes(('KEY:'+str(self.generation)+'\n').encode())
        elif args[0]=='openssl':
            if self.expired:raise subprocess.CalledProcessError(1,args)
        else:self.command(*args)
        return SimpleNamespace(returncode=0)

    def command(self,*args):
        self.commands.append(args)
        if args[:4]==('incus','config','trust','add-certificate'):
            fingerprint=hashlib.sha256(b'DER:'+Path(args[4]).read_bytes()).hexdigest()
            self.trusted.append({'type':'metrics','fingerprint':fingerprint})
            self.assertIn('--type=metrics',args)
        elif args[:4]==('incus','config','trust','remove'):
            self.trusted[:]=[entry for entry in self.trusted if entry['fingerprint']!=args[4]]
        elif args[:4]==('incus','config','set','core.metrics_address'):
            self.settings['core.metrics_address']=args[4]
        elif args[:4]==('incus','config','unset','core.metrics_address'):
            self.settings.pop('core.metrics_address',None)
        elif args[0]=='runuser':
            self.assertEqual(args[1:4],('-u','homecloud','--'))
            self.assertEqual(args[-1],'check-metrics')
            config=json.loads(Path(args[-2]).read_text())
            self.assertEqual(config['metrics']['server_name'],'incus.local')
            self.assertTrue(Path(config['metrics']['client_identity']).is_file())
            if self.fail_probe:raise subprocess.CalledProcessError(1,args)
        else:self.fail('Unexpected command: '+str(args))

    def setup(self,**kwargs):
        return installer.setup_metrics(Path('/usr/local/bin/homecloudd'),config_dir=self.directory,**kwargs)

    def test_verified_setup_is_scoped_private_and_idempotent(self):
        self.setup()
        config=json.loads(self.control.read_text())
        self.assertEqual(config['other_setting'],'preserved')
        self.assertEqual(config['metrics']['port'],8444)
        self.assertEqual(self.settings['core.metrics_address'],'127.0.0.1:8444')
        self.assertEqual(len(self.trusted),1)
        for path in [self.control,Path(config['metrics']['client_identity']),Path(config['metrics']['server_certificate'])]:
            self.assertEqual(path.stat().st_mode&0o777,0o640)
        self.assertEqual(Path(config['metrics']['client_identity']).parent.stat().st_mode&0o777,0o750)
        original=self.control.read_bytes();self.setup()
        self.assertEqual(original,self.control.read_bytes())
        self.assertEqual(self.generation,1)
        self.assertEqual(len(self.trusted),1)
        self.assertFalse(any('systemctl' in command for command in self.commands))

    def test_preflight_is_read_only(self):
        self.setup(check=True)
        self.assertEqual(self.control.read_bytes(),self.original)
        self.assertFalse((self.directory/'metrics').exists())
        self.assertEqual(self.commands,[])

    def test_expiring_identity_fails_read_only_preflight_before_maintenance(self):
        self.setup();original=self.control.read_bytes();self.commands.clear();self.expired=True
        with self.assertRaisesRegex(RuntimeError,'expires soon'):self.setup(check=True)
        self.assertEqual(self.control.read_bytes(),original)
        self.assertEqual(self.commands,[])
        installer.metrics_preflight(config_dir=self.directory,rotate=True)

    def test_existing_custom_port_is_reused_when_not_explicitly_overridden(self):
        self.setup(port=9444);original=self.control.read_bytes();self.commands.clear()
        self.setup(check=True);self.setup()
        self.assertEqual(self.control.read_bytes(),original)
        self.assertEqual(self.settings['core.metrics_address'],'127.0.0.1:9444')
        with self.assertRaisesRegex(RuntimeError,'unrelated'):self.setup(port=8444,check=True)

    def test_missing_identity_is_reported_without_claiming_it_is_expiring(self):
        self.setup();original=self.control.read_bytes();self.commands.clear()
        Path(json.loads(original)['metrics']['client_identity']).unlink()
        with self.assertRaisesRegex(RuntimeError,'identity is missing'):self.setup(check=True)
        self.assertEqual(self.control.read_bytes(),original)
        self.assertEqual(self.commands,[])

    def test_metrics_failure_still_resumes_legacy_guests_and_controller(self):
        commands=[]
        with patch.object(installer,'setup_metrics',side_effect=RuntimeError('metrics unavailable')), \
             patch.object(installer,'incus_query',return_value={'status':'Stopped'}), \
             patch.object(installer,'run',side_effect=lambda *args:commands.append(args)):
            with self.assertRaisesRegex(RuntimeError,'metrics unavailable'):
                installer.setup_metrics_and_resume(Path('/usr/local/bin/homecloudd'),None,False,['legacy-test'])
        self.assertEqual(commands,[('incus','start','legacy-test'),('systemctl','restart','homecloudd.service')])

    def test_controller_restart_is_attempted_even_when_legacy_resume_fails(self):
        commands=[]
        def fail_start(*args):
            commands.append(args)
            if args[0]=='incus':raise RuntimeError('legacy start failed')
        with patch.object(installer,'setup_metrics'), \
             patch.object(installer,'incus_query',return_value={'status':'Stopped'}), \
             patch.object(installer,'run',side_effect=fail_start):
            with self.assertRaisesRegex(RuntimeError,'legacy start failed'):
                installer.setup_metrics_and_resume(Path('/usr/local/bin/homecloudd'),None,False,['legacy-test'])
        self.assertEqual(commands[-1],('systemctl','restart','homecloudd.service'))

    def test_optional_null_configuration_is_supported(self):
        config=json.loads(self.control.read_text());config['metrics']=None
        self.control.write_text(json.dumps(config));self.setup()
        self.assertIsInstance(json.loads(self.control.read_text())['metrics'],dict)

    def test_reused_credential_permissions_are_repaired(self):
        self.setup();identity=Path(json.loads(self.control.read_text())['metrics']['client_identity'])
        identity.chmod(0o644);identity.parent.chmod(0o755)
        self.setup()
        self.assertEqual(identity.stat().st_mode&0o777,0o640)
        self.assertEqual(identity.parent.stat().st_mode&0o777,0o750)

    def test_rejects_insecure_or_foreign_listener_without_changes(self):
        for settings in [{'core.metrics_authentication':'false'},{'core.metrics_address':':8444'},
                         {'core.metrics_address':'127.0.0.1:9999'}]:
            self.settings=settings
            with self.assertRaises(RuntimeError):self.setup()
            self.assertEqual(self.control.read_bytes(),self.original)
            self.assertEqual(self.commands,[])

    def test_probe_failure_rolls_back_only_new_trust_and_listener(self):
        self.fail_probe=True
        with self.assertRaises(subprocess.CalledProcessError):self.setup()
        self.assertEqual(self.control.read_bytes(),self.original)
        self.assertEqual(self.trusted,[])
        self.assertNotIn('core.metrics_address',self.settings)
        self.assertEqual(list((self.directory/'metrics').iterdir()),[])

    def test_existing_listener_and_identity_survive_probe_failure(self):
        self.setup();original=self.control.read_bytes();trusted=list(self.trusted)
        self.fail_probe=True
        with self.assertRaises(subprocess.CalledProcessError):self.setup()
        self.assertEqual(self.control.read_bytes(),original)
        self.assertEqual(self.trusted,trusted)
        self.assertEqual(self.settings['core.metrics_address'],'127.0.0.1:8444')

    def test_never_reuses_a_management_capable_certificate(self):
        self.setup();original=self.control.read_bytes();self.trusted[0]['type']='client'
        with self.assertRaisesRegex(RuntimeError,'management-capable'):self.setup()
        self.assertEqual(self.control.read_bytes(),original)
        self.assertEqual(self.trusted[0]['type'],'client')

    def test_rotation_preserves_old_identity_until_controller_restart(self):
        self.setup();old=json.loads(self.control.read_text())['metrics']['client_identity']
        self.expired=True
        with self.assertRaisesRegex(RuntimeError,'expires soon'):self.setup()
        self.setup(rotate=True)
        self.assertNotEqual(json.loads(self.control.read_text())['metrics']['client_identity'],old)
        self.assertTrue(Path(old).is_file())
        self.assertEqual(len(self.trusted),2)

    def test_failed_atomic_config_write_retains_previous_config_and_rolls_back(self):
        original=installer.metrics_file
        def fail(path,data,gid):
            if path==self.control:raise OSError('injected config write failure')
            original(path,data,gid)
        with patch.object(installer,'metrics_file',fail):
            with self.assertRaisesRegex(OSError,'injected'):self.setup()
        self.assertEqual(self.control.read_bytes(),self.original)
        self.assertEqual(self.trusted,[])
        self.assertNotIn('core.metrics_address',self.settings)
        self.assertEqual(list((self.directory/'metrics').iterdir()),[])

    def test_managed_credentials_do_not_follow_directory_symlinks(self):
        (self.directory/'metrics').symlink_to(self.directory,target_is_directory=True)
        with self.assertRaisesRegex(RuntimeError,'symlink'):self.setup()
        self.assertEqual(self.commands,[])


if __name__=='__main__':unittest.main()
