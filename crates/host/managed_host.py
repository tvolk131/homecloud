"""Fixed administrator host workload: backed block disk and root installer only."""
import hashlib
import json
from pathlib import Path
import uuid

DISK_GIB=32
BUNDLE=Path('/usr/local/libexec/homecloud/homecloud-host-bundle.tar.gz')


def enabled(vm):return 'homecloud_host' in vm['spec'].get('applications',[])
def volume(vm):return 'hc-lab-'+uuid.UUID(vm['id']).hex
def wwn(vm):return '0x5000'+uuid.UUID(vm['id']).hex[:12]


def device(runtime,c,vm,create=False):
    if not enabled(vm):return {}
    assert vm['resources']['disk_bytes']>=(vm['spec']['disk_gib']+DISK_GIB+c.get('disk_overhead_gib_per_vm',2))*runtime.GIB,'conflict: host test disk is not backed'
    pool=runtime.storage_pool(vm['owner']);path='/1.0/storage-pools/'+pool+'/volumes/custom/'+volume(vm)
    record=runtime.api('GET',path,missing=True)
    if record is None:
        assert create,'conflict: managed host test volume is missing'
        runtime.api('POST','/1.0/storage-pools/'+pool+'/volumes',{'name':volume(vm),'type':'custom','content_type':'block',
            'config':{'size':str(DISK_GIB)+'GiB','user.homecloud.id':vm['id'],'user.homecloud.owner':vm['owner']}})
        record=runtime.api('GET',path)
    assert record['content_type']=='block' and record['config'].get('user.homecloud.id')==vm['id'] and record['config'].get('user.homecloud.owner')==vm['owner'],'conflict: foreign managed host disk'
    return {'hostpool':{'type':'disk','source':volume(vm),'pool':pool,'io.bus':'virtio-scsi','wwn':wwn(vm)}}


def cleanup(runtime,vm):
    if not enabled(vm):return
    # Called only after the owned instance has been deleted. Never follow a
    # guest-provided path or delete another VM's volume on an ambiguous retry.
    path='/1.0/storage-pools/'+runtime.storage_pool(vm['owner'])+'/volumes/custom/'+volume(vm)
    record=runtime.api('GET',path,missing=True)
    if record is None:return
    assert record['config'].get('user.homecloud.id')==vm['id'] and record['config'].get('user.homecloud.owner')==vm['owner'],'conflict: foreign managed host disk'
    assert not record.get('used_by'),'conflict: managed host disk is still attached'
    runtime.api('DELETE',path)


def application(runtime,vm,app,action):
    assert enabled(vm) and app['vm_id']==vm['id'] and app['kind']=='homecloud_host'
    assert app['desired_version']=='0.1.0' and app['start_on_boot'] is True
    runtime.owned(vm);assert runtime.state(vm)=='running','VM is not running'
    name=runtime.vm_name(vm);script='/usr/local/libexec/homecloud/managed-host.py'
    def guest(*args,**kwargs):return runtime.run('incus','exec',name,'--',*args,timeout=kwargs.pop('timeout',15),**kwargs)
    source=Path(__file__).with_name('managed-host.py').read_text()
    writer="from pathlib import Path;import sys; p=Path('/usr/local/libexec/homecloud/managed-host.py');p.parent.mkdir(parents=True,exist_ok=True);p.write_text(sys.stdin.read());p.chmod(0o644)"
    guest('python3','-c',writer,input=source)
    report=json.loads(guest('python3',script,'inspect').stdout)
    op=action.get('operation')
    if not op:return report
    assert op['application_id']==app['id'] and op['kind']=='install','Host workload has no account setup hook'
    op_id=str(uuid.UUID(op['id']));unit='homecloud-host-install-'+uuid.UUID(app['id']).hex
    active=guest('systemctl','is-active','--quiet',unit,check=False).returncode==0
    previous=report.get('job')
    if active:
        report['job']={'id':op_id,'phase':'running'};return report
    if action['kind']=='inspect':
        if not previous or previous['id']!=op_id or previous['phase']=='running':report['job']={'id':op_id,'phase':'interrupted'}
        return report
    assert action['kind']=='ensure_installed'
    if previous and previous['id']==op_id and previous['phase'] in ('succeeded','failed'):return report
    assert BUNDLE.is_file(),'Install the reviewed Homecloud host bundle on this host first'
    digest=hashlib.sha256(BUNDLE.read_bytes()).hexdigest()
    existing=guest('sha256sum','/opt/homecloud-host-bundle.tar.gz',check=False).stdout.split()
    if not existing or existing[0]!=digest:
        runtime.run('incus','file','push',str(BUNDLE),name+'/opt/homecloud-host-bundle.tar.gz',timeout=120)
    guest('systemd-run','--quiet','--collect','--unit='+unit,'--property=UMask=0077','--property=RuntimeMaxSec=2400',
          'python3',script,'install',op_id,digest,wwn(vm),timeout=15)
    return json.loads(guest('python3',script,'inspect').stdout)
