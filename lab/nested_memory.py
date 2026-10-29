"""Full resident guest budgets in a disposable nested Incus/ZFS worker.
This is an isolated component test, not full saturation of the physical server.
"""
from pathlib import Path
import json,os,re,subprocess,sys,time,tarfile
from persistence_guest import h,ROOT
import arc_policy as arc
GIB=1024**3

def run(*args,timeout=90,check=True,input=None):
 return subprocess.run(args,input=input,text=True,check=check,capture_output=True,timeout=timeout)
def get(path):return json.loads(run('incus','query',path).stdout)
def residency(name):
 pid=get('/1.0/instances/'+name+'/state')['pid'];text=Path('/proc',str(pid),'smaps').read_text();guest=False;rss=0;total=0
 for line in text.splitlines():
  if re.match(r'^[0-9a-f]+-[0-9a-f]+ ',line):guest='memfd:' in line
  if line.startswith('Rss:'):
   value=int(line.split()[1])*1024;total+=value
   if guest:rss+=value
 return {'configured_memory_bytes':GIB,'qemu_rss_bytes':total,'guest_backing_resident_bytes':rss}
def stats():
 m={l.split(':')[0]:int(l.split()[1])*1024 for l in Path('/proc/meminfo').read_text().splitlines() if l.split(':')[0] in ('MemTotal','MemAvailable','Cached','Shmem','SwapTotal','SwapFree')}
 v={k:int(value) for k,value in map(str.split,Path('/proc/vmstat').read_text().splitlines()) if k in ('pswpout','oom_kill')}
 return {'memory':m,'vmstat':v,'arc':arc.observe(),'pressure':Path('/proc/pressure/memory').read_text()}

def main():
 assert Path('/dev/kvm').exists()
 swapfile=ROOT/'worker-swapfile';made_swap=False
 if stats()['memory']['SwapTotal']==0:
  run('fallocate','-l','2G',str(swapfile));swapfile.chmod(0o600);run('mkswap',str(swapfile));run('swapon',str(swapfile));made_swap=True
 total=stats()['memory']['MemTotal'];whole=total//GIB;guest_gib=whole-4
 assert guest_gib in (1,2),'Review this fixture for its current VM size'
 config={'capacity':{'memory_bytes':guest_gib*GIB},'minimum_reserves':{'memory_bytes':4*GIB},'memory_operating_floor_bytes':2*GIB,'arc_recommended_maximum_bytes':2*GIB,'arc_minimum_limit_bytes':256*1024**2}
 result={'scope':'disposable nested worker, stock guest distro Incus/QEMU','before':stats(),'operating_floor_bytes':2*GIB,'system_reserve_bytes':4*GIB,'guest_budget_bytes':guest_gib*GIB,'rounds':[]}
 def save():h.save(ROOT/'nested-memory-evidence.json',result)
 save()
 # All devices, pools, and names below exist only inside this marked VM.
 image=ROOT/'nested-pool.img';run('truncate','-s','16G',str(image))
 assert run('zpool','list','hcproof',check=False).returncode!=0,'Existing nested test needs inspection'
 loop=run('losetup','--find','--show','--direct-io=on','--sector-size','4096',str(image)).stdout.strip()
 loop_info=json.loads(run('losetup','--json','--output','NAME,BACK-FILE,DIO',loop).stdout)['loopdevices'][0]
 assert loop_info['back-file']==str(image) and loop_info['dio'] in (True,1,'1'),loop_info
 result['vdev']={'mode':'loop block device, direct I/O verified','info':loop_info};save()
 run('zpool','create','-f','-o','cachefile=none','hcproof',loop)
 run('zfs','create','-o','compression=off','-o','quota=8G','-o','mountpoint=/var/lib/homecloud-recovery-proof/io','hcproof/load')
 preseed='''config: {}
networks:
- name: incusbr0
  type: bridge
  config:
    ipv4.address: 10.188.33.1/24
    ipv4.nat: "true"
    ipv6.address: none
storage_pools:
- name: proof
  driver: zfs
  config:
    source: hcproof/incus
    volume.zfs.reserve_space: "false"
profiles:
- name: default
  config: {}
  devices: {}
'''
 run('zfs','create','hcproof/incus');run('incus','admin','init','--preseed',input=preseed)
 files=list(Path('/opt/homecloud-recovery-proof/image').iterdir())
 files.sort(key=lambda f:(not tarfile.is_tarfile(f),f.name))
 run('incus','image','import',*[str(f) for f in files],'--alias','proof-base',timeout=180)
 vms=[]
 for i in range(guest_gib):
  name='resident-proof-'+str(i);vms.append(name)
  payload={'name':name,'type':'virtual-machine','profiles':[],'source':{'type':'image','alias':'proof-base'},'config':{'limits.memory':'1GiB','limits.cpu':'1','raw.qemu':'-mem-prealloc','cloud-init.user-data':'#cloud-config\nruncmd:\n - [touch, /etc/homecloud-nested-proof]\n'},'devices':{'root':{'type':'disk','path':'/','pool':'proof','size':'10GiB'},'eth0':{'type':'nic','network':'incusbr0'}}}
  run('incus','query','-X','POST','/1.0/instances','-d',json.dumps(payload),'--wait');run('incus','start',name,timeout=120)
  end=time.monotonic()+240
  while time.monotonic()<end:
   p=run('incus','exec',name,'--','cloud-init','status','--format=json',check=False,timeout=15)
   try:done=json.loads(p.stdout).get('status')=='done'
   except ValueError:done=False
   if done:break
   time.sleep(2)
  else:raise RuntimeError('Nested guest setup failed: '+p.stderr+p.stdout)
 result['versions']={a:run(*cmd).stdout.strip() for a,cmd in {'incus':['incus','version'],'qemu':['qemu-system-x86_64','--version'],'zfs':['zfs','version'],'kernel':['uname','-r']}.items()}
 # -mem-prealloc faults in every configured byte regardless of idle guest RSS.
 resident={}
 for name in vms:
  resident[name]=residency(name)
  assert resident[name]['guest_backing_resident_bytes']>=GIB,(name,resident[name])
 result['resident_guests']=resident
 guest_load="import json,time;from pathlib import Path\nM=1024**2\nmem={l.split(':')[0]:int(l.split()[1])*1024 for l in Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')}\ntarget=max(0,mem['MemAvailable']-128*M);chunks=[bytearray(16*M) for _ in range(target//(16*M))];end=time.monotonic()+1500\nPath('/tmp/homecloud-nested-allocation.json').write_text(json.dumps({'allocated_bytes':len(chunks)*16*M}))\nwhile time.monotonic()<end:\n for block in chunks:\n  for i in range(0,len(block),4096):block[i]=(block[i]+1)%251\n time.sleep(3)"
 for name in vms:run('incus','exec',name,'--','systemd-run','--unit=resident-memory-proof','--collect','--property=RuntimeMaxSec=1550','python3','-c',guest_load)
 result['guest_working_sets']={}
 for name in vms:
  end=time.monotonic()+30
  while time.monotonic()<end:
   p=run('incus','exec',name,'--','cat','/tmp/homecloud-nested-allocation.json',check=False)
   if p.returncode==0:result['guest_working_sets'][name]=json.loads(p.stdout);break
   time.sleep(.5)
  else:raise RuntimeError('Nested memory load did not become ready')
 save()
 path='/var/lib/homecloud-recovery-proof/io';run('dd','if=/dev/zero','of='+path+'/warm','bs=16M','count=192','conv=fdatasync','status=none',timeout=180)
 code="import os,time;end=time.monotonic()+1500;p="+repr(path)+";data=b'x'*(8*1024**2)\nwhile time.monotonic()<end:\n with open(p+'/writes','wb',buffering=0) as f:\n  for _ in range(64):f.write(data)\n  os.fsync(f.fileno())\n with open(p+'/warm','rb',buffering=0) as f:\n  while f.read(8*1024**2):pass\n time.sleep(.1)"
 io=subprocess.Popen(['python3','-c',code],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,text=True)
 try:
  for mode in ['automatic','custom']:
   selected={'mode':mode} if mode=='automatic' else {'mode':mode,'max_bytes':2*GIB}
   arc.apply(h,config,selected);before=stats();samples=[];start=time.monotonic()
   while time.monotonic()-start<600:
    sample=stats();sample['elapsed_seconds']=round(time.monotonic()-start,2);samples.append(sample)
    assert sample['vmstat']['oom_kill']==before['vmstat']['oom_kill'] and sample['arc']['maximum_bytes']==2*GIB
    if len(samples)%12==1:
     sample['residency']={name:residency(name) for name in vms}
     assert all(v['guest_backing_resident_bytes']>=GIB for v in sample['residency'].values()),sample['residency']
    assert io.poll() is None,'Storage load exited early'
    assert all(get('/1.0/instances/'+n+'/state')['status']=='Running' for n in vms)
    result['rounds']=[r for r in result['rounds'] if r['mode']!=mode]+[{'mode':mode,'before':before,'samples':samples}];save();time.sleep(5)
   last=samples[-1];swap_out_bytes=(last['vmstat']['pswpout']-before['vmstat']['pswpout'])*os.sysconf('SC_PAGE_SIZE')
   psi=max(float(x) for x in re.findall(r'avg10=([0-9.]+)',last['pressure']))
   assert all(run('incus','exec',n,'--','systemctl','is-active','resident-memory-proof.service').stdout.strip()=='active' for n in vms)
   assert swap_out_bytes<=total//100 and psi<1 and 'healthy' in run('zpool','status','-x','hcproof').stdout
   result['rounds'][-1].update(swap_out_bytes=swap_out_bytes,final_psi_avg10=psi,passed=True);save()
  target=dict(config,minimum_reserves={'memory_bytes':3*GIB},arc_recommended_maximum_bytes=GIB)
  arc.apply(h,target,{'mode':'custom','max_bytes':GIB})
  record={};arc.reclaim_for_release(target,record,lambda value:h.save(ROOT/'memory-reclaim.json',value))
  assert arc.observe()['size_bytes']<=GIB and 'arc_previous_sys_free' not in record
  result['cache_release']={'allowance_bytes':GIB,'observed':arc.observe()};result['passed']=True;save()
 finally:
  io.terminate()
  try:io.wait(timeout=15)
  except subprocess.TimeoutExpired:io.kill();io.wait()
 # Cleanup is deliberately confined to recorded nested names/pool on success.
 for name in vms:run('incus','delete',name,'--force')
 run('incus','storage','delete','proof');run('zpool','destroy','hcproof');run('losetup','--detach',loop);image.unlink()
 if made_swap:run('swapoff',str(swapfile));swapfile.unlink()
 result['nested_cleaned']=True;save()
 print(json.dumps(result))
if __name__=='__main__':main()
