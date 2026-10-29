"""Dedicated test disks use the normal backing ledger and immutable owner tags."""
import copy
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
import managed_host as m
GIB=1024**3

class HostDiskTests(unittest.TestCase):
    def setUp(self):
        self.vm={'id':'00000000-0000-4000-8000-000000000001','owner':'00000000-0000-4000-8000-000000000002',
            'spec':{'applications':['homecloud_host'],'disk_gib':32},'resources':{'disk_bytes':66*GIB}}
        self.volume=None;self.creates=0;self.deletes=0;self.lose_reply=True
        def api(method,path,data=None,**kwargs):
            if method=='GET':return self.volume
            if method=='POST':
                self.creates+=1;self.volume=copy.deepcopy(data)
                if self.lose_reply:self.lose_reply=False;raise RuntimeError('lost create acknowledgement')
            if method=='DELETE':self.deletes+=1;self.volume=None
        self.host=SimpleNamespace(GIB=GIB,api=api,storage_pool=lambda _: 'owned')
    def test_lost_volume_reply_reuses_same_owned_disk_without_second_create(self):
        with self.assertRaisesRegex(RuntimeError,'lost create'):m.device(self.host,{},self.vm,create=True)
        d=m.device(self.host,{},self.vm,create=True)
        self.assertEqual(self.creates,1);self.assertEqual(d['hostpool']['wwn'],m.wwn(self.vm))
    def test_no_unbacked_disk_and_no_foreign_or_still_used_volume_deletion(self):
        self.vm['resources']['disk_bytes']=34*GIB
        with self.assertRaisesRegex(AssertionError,'not backed'):m.device(self.host,{},self.vm,create=True)
        self.assertEqual(self.creates,0)
        self.volume={'config':{'user.homecloud.id':'foreign'}}
        with self.assertRaisesRegex(AssertionError,'foreign'):m.cleanup(self.host,self.vm)
        self.volume={'config':{'user.homecloud.id':self.vm['id'],'user.homecloud.owner':self.vm['owner']},'used_by':['a VM']}
        with self.assertRaisesRegex(AssertionError,'still attached'):m.cleanup(self.host,self.vm)
        self.assertEqual(self.deletes,0)


class BootstrapIsolationTests(unittest.TestCase):
    def test_inspection_never_overwrites_the_inner_host_software(self):
        import io,json,tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);protected=root/'usr/local/libexec/homecloud/managed-host.py';protected.parent.mkdir(parents=True);protected.write_text('inner software')
            source=Path(__file__).resolve().parents[3]/'guest/managed-host.py'
            calls=[]
            def run(*args,**kwargs):
                command=args[4:];calls.append(command)
                if command[:2]==('python3','-c'):
                    real_path=type(root)
                    def mapped(value):return real_path(temp)/str(value).lstrip('/')
                    with patch('pathlib.Path',side_effect=mapped),patch('sys.stdin',io.StringIO(kwargs['input'])):exec(command[2],{})
                    return SimpleNamespace(stdout='')
                self.assertEqual(command,('python3',m.BOOTSTRAP,'inspect'));return SimpleNamespace(stdout=json.dumps({'observation':{'service':'running'},'job':None}))
            runtime=SimpleNamespace(owned=lambda vm:None,state=lambda vm:'running',vm_name=lambda vm:'fixture',run=run)
            vm={'id':'00000000-0000-4000-8000-000000000001','spec':{'applications':['homecloud_host']}}
            app={'vm_id':vm['id'],'kind':'homecloud_host','desired_version':'0.1.0','start_on_boot':True}
            with patch.object(m,'BOOTSTRAP_SOURCE',source):report=m.application(runtime,vm,app,{'kind':'inspect'})
            self.assertEqual(report['observation']['service'],'running');self.assertEqual(protected.read_text(),'inner software')
            self.assertEqual((root/m.BOOTSTRAP.lstrip('/')).read_text(),source.read_text())
            self.assertEqual(len(calls),2)
