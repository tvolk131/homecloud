"""Real ARC/initramfs crash recovery, restricted to an explicitly marked disposable VM.
CPU/systemd placement is excluded here; those live-host fences have separate proofs.
"""
from pathlib import Path
import importlib,json,os,signal,subprocess,sys,time,uuid
assert os.geteuid()==0 and Path('/etc/homecloud-recovery-proof').is_file()
assert subprocess.run(['systemd-detect-virt','--vm','--quiet']).returncode==0,'Refusing to run this fault fixture on a physical host'
sys.path.insert(0,'/opt/homecloud-recovery-proof');import resource_reserves as r
ROOT=Path('/var/lib/homecloud-recovery-proof');ROOT.mkdir(exist_ok=True,mode=0o700)
GIB=1024**3
class Host:
 STATE=ROOT
 @staticmethod
 def save(path,value):
  temp=path.with_suffix('.tmp')
  with temp.open('w') as f:json.dump(value,f);f.flush();os.fsync(f.fileno())
  temp.chmod(0o600);temp.replace(path)
 @staticmethod
 def run(*args,**kwargs):return subprocess.run(args,check=True,capture_output=True,text=True,**kwargs)
h=Host();r.enforce=lambda *a,**k:None
base={'capacity':{'memory_bytes':2*GIB,'cpu_cores':1,'disk_bytes':GIB},'cpu_groups':[[0,1]],'housekeeping':[],
 'minimum_pool_available_bytes':32*1024**2,'minimum_reserves':{'memory_bytes':768*1024**2,'cpu_cores':0,'disk_bytes':32*1024**2},
 'memory_operating_floor_bytes':512*1024**2,'arc_recommended_maximum_bytes':256*1024**2,'arc_minimum_limit_bytes':256*1024**2}
def main():
    action=sys.argv[1]

    if action=='inject':
     point=sys.argv[2];assert point in ('before-initramfs','during-initramfs','after-initramfs')
     # Set a known bootable previous policy before injecting a torn update.
     old=dict(base,minimum_reserves=dict(base['minimum_reserves'],memory_bytes=GIB))
     r.arc_policy.apply(h,old,{'mode':'custom','max_bytes':512*1024**2})
     state=r.initial()['state'];state.update(revision=1,arc={'mode':'automatic'})
     record={'state':state,'phase':'applying','operation_id':str(uuid.uuid4()),'discarded':[],'arc_persistence_required':True}
     knob=r.arc_policy.PARAMS/'zfs_arc_sys_free';record['arc_previous_sys_free']=int(knob.read_text());r.write_record(h,record)
     knob.write_text(str(64*1024**2))
     def crash_run(*args,**kwargs):
      if args[0]=='update-initramfs':
       if point=='after-initramfs':Host.run(*args,**kwargs)
       elif point=='during-initramfs':
        child=subprocess.Popen(args,start_new_session=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
        # The actual tool has begun generating an image; kill its entire process
        # group while the staged build is in progress, only in this disposable VM.
        line=child.stdout.readline();assert 'Generating' in line,line
        time.sleep(.2);assert child.poll() is None,'Build finished before injection'
        os.killpg(child.pid,signal.SIGKILL);child.wait()

       os.kill(os.getpid(),signal.SIGKILL)
      return Host.run(*args,**kwargs)
     h.run=crash_run
     r.arc_policy.apply(h,r.effective(base,record),state['arc'])
     raise AssertionError('Crash injection did not happen')
    elif action=='recover':
     record=r.read_record(h,base);r.boot(h,r.effective(base,record),[])
     assert r.read_record(h,base)['phase']=='applied'
    else:assert action=='inspect'
    record=r.read_record(h,base);arc=r.arc_policy.observe()
    print(json.dumps({'journal':record,'arc':arc,'temporary_reclaim_target':int((r.arc_policy.PARAMS/'zfs_arc_sys_free').read_text()),
     'boot':Path('/proc/sys/kernel/random/boot_id').read_text().strip()}))

if __name__=='__main__':main()
