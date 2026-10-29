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
