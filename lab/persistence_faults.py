"""Real filesystem and virtual hard-power-loss faults in a marked VM only."""
from pathlib import Path
import hashlib,json,os,subprocess,sys,time,uuid
from persistence_guest import h,r,base,ROOT
GIB=1024**3
POINT=ROOT/'power-checkpoint.json'
def run(*args,**kw):return subprocess.run(args,check=True,capture_output=True,text=True,**kw)
def prepare():
    POINT.unlink(missing_ok=True)
    old=dict(base,minimum_reserves=dict(base['minimum_reserves'],memory_bytes=GIB))
    r.arc_policy.apply(h,old,{'mode':'custom','max_bytes':512*1024**2})
    state=r.initial()['state'];state.update(revision=1,arc={'mode':'automatic'})
    record={'state':state,'phase':'applying','operation_id':str(uuid.uuid4()),'discarded':[],'arc_persistence_required':True}
    record['arc_previous_sys_free']=int((r.arc_policy.PARAMS/'zfs_arc_sys_free').read_text())
    r.write_record(h,record);(r.arc_policy.PARAMS/'zfs_arc_sys_free').write_text(str(64*1024**2))
    return record

def snapshot():
    record=r.read_record(h,base)
    return {'journal':record,'boot':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),'arc':r.arc_policy.observe(),'temporary_reclaim_target':int((r.arc_policy.PARAMS/'zfs_arc_sys_free').read_text())}

def power(point):
    assert point in ('before-initramfs','during-initramfs','after-initramfs')
    record=prepare()
    def pause(*args,**kw):
        if args[0]!='update-initramfs':return run(*args,**kw)
        if point=='during-initramfs':
            child=subprocess.Popen(args,start_new_session=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
            line=child.stdout.readline();assert 'Generating' in line,line
            assert child.poll() is None
        elif point=='after-initramfs':run(*args,**kw)
        h.save(POINT,dict(point=point,attempt=sys.argv[3],before=snapshot()))
        time.sleep(600)
        raise AssertionError('Expected virtual power-off did not arrive')
    h.run=pause;r.arc_policy.apply(h,r.effective(base,record),record['state']['arc'])

def filesystem(mode):
    assert mode in ('boot-full','config-readonly')
    record=prepare();mounted=[];before=snapshot();error=None
    try:
        if mode=='config-readonly':
            run('mount','--bind','/etc/modprobe.d','/etc/modprobe.d');mounted.append('/etc/modprobe.d')
            run('mount','-o','remount,bind,ro','/etc/modprobe.d')
        else:
            image=ROOT/'boot-full.ext4';mount=ROOT/'boot-full';mount.mkdir(exist_ok=True)
            run('truncate','-s','512M',str(image));run('mkfs.ext4','-q','-F','-m','0',str(image))
            run('mount','-o','loop',str(image),str(mount));mounted.append(str(mount))
            run('cp','-a','/boot/.',str(mount))
            st=os.statvfs(mount);remaining=st.f_bavail*st.f_frsize
            assert remaining>2*1024**2
            with (mount/'homecloud-proof-fill').open('wb',buffering=0) as f:
                for _ in range((remaining-512*1024)//(1024**2)):f.write(b'x'*(1024**2))
                os.fsync(f.fileno())
            run('mount','--bind',str(mount),'/boot');mounted.append('/boot')
        try:r.arc_policy.apply(h,r.effective(base,record),record['state']['arc'])
        except (OSError,subprocess.CalledProcessError,AssertionError) as e:error=str(e)
        assert error,'Expected persistence failure did not occur'
        held=snapshot();assert held['journal']['phase']=='applying' and held['journal']['arc_persistence_required']
        # Boot recovery must also refuse to acknowledge while the fault persists.
        try:r.boot(h,r.effective(base,held['journal']),[])
        except (OSError,subprocess.CalledProcessError,AssertionError):pass
        assert snapshot()['journal']['phase']=='applying'
        return {'fault':mode,'before':before,'during':held,'error':error,'failed_closed':True}
    finally:
        for mount in reversed(mounted):run('umount',mount)
        if mode=='boot-full':(ROOT/'boot-full.ext4').unlink(missing_ok=True)

if __name__=='__main__':
    if sys.argv[1]=='power':power(sys.argv[2])
    elif sys.argv[1]=='filesystem':print(json.dumps(filesystem(sys.argv[2])))
    elif sys.argv[1]=='inspect':print(json.dumps(snapshot()))
    else:raise ValueError('Unknown fixture action')
