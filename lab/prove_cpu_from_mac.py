"""Run the static CPU prototype, reboot persistence, and GPU compatibility.

Requires the earlier storage fixtures to be cleaned up. Uses only the
explicitly authorized host and two tagged lab VMs; host reboot is intentional.
"""
import json
from pathlib import Path
import subprocess
import sys
import time
from remote import host, save, script, stream_script, upload, ROOT

upload('qemu_cpu_launcher.c','configure_cpu.py','prove_cpu.py','cuda_smoke.py')
if '--existing-layout' not in sys.argv:
    stream_script('configure_cpu.py','install',timeout=900)
stream_script('prove_cpu.py',timeout=1200)
before=host(['cat','/proc/sys/kernel/random/boot_id']).stdout.strip()
print('CPU lifecycle tests passed; rebooting host to verify persistent policy',flush=True)
host(['sudo','-n','incus','stop','hclab-vm01','--timeout','30'])
host(['sudo','-n','zpool','sync','hclab'])
unit='home-cloud-proof-reboot-'+str(int(time.time()))
host(['sudo','-n','systemd-run','--unit='+unit,'--on-active=4s','/usr/bin/systemctl','reboot'])
deadline=time.monotonic()+240
after=None
while time.monotonic()<deadline:
    time.sleep(3)
    try:
        p=host(['cat','/proc/sys/kernel/random/boot_id'],timeout=10,check=False)
        if p.returncode==0 and p.stdout.strip()!=before:
            after=p.stdout.strip();break
    except subprocess.TimeoutExpired:pass
assert after and after!=before,'Host did not return with a new boot ID'
deadline=time.monotonic()+180
while time.monotonic()<deadline:
    p=host(['sudo','-n','incus','query','/1.0/instances/hclab-vm01/state'],check=False)
    if p.returncode==0 and json.loads(p.stdout)['status']=='Running':break
    time.sleep(3)
else:raise RuntimeError('VM01 did not autostart after reboot')
host(['sudo','-n','incus','start','hclab-vm02'])
print('Host returned; checking CPU placement and load after reboot',flush=True)
stream_script('prove_cpu.py','--post-reboot',timeout=400)
save('cpu-host-reboot.json',{'before_boot_id':before,'after_boot_id':after,'vm01_autostarted':True})

print('Checking GPU compute with CPU fencing active',flush=True)
gpu_code=r'''
import json,subprocess,time
from pathlib import Path
r={}
def run(*a,input=None,check=True):
 p=subprocess.run(a,input=input,capture_output=True,text=True,timeout=180)
 if check and p.returncode:raise RuntimeError(str(a)+p.stdout+p.stderr)
 return p
helper='/usr/local/libexec/home-cloud-lab/gpu_passthrough.py'
try:
 run('python3',helper,'attach')
 end=time.monotonic()+150
 while time.monotonic()<end:
  p=run('incus','exec','hclab-vm02','--','nvidia-smi','--query-gpu=name,uuid,driver_version','--format=csv,noheader',check=False)
  if p.returncode==0:r['gpu']=p.stdout.strip();break
  time.sleep(2)
 else:raise RuntimeError('GPU guest driver not ready')
 p=run('incus','exec','hclab-vm02','--user','1000','--env','HOME=/home/dev','--','python3','-',input=Path('/usr/local/libexec/home-cloud-lab/cuda_smoke.py').read_text())
 r['compute']=json.loads(p.stdout);assert r['compute']['correct_result']
 r['guest_uid']=1000
finally:
 p=run('python3',helper,'release',check=False)
 r['release_exit_code']=p.returncode;r['release_error']=p.stderr
 r['release']=json.loads(Path('/var/lib/home-cloud-lab/gpu-handoff-state.json').read_text())
 Path('/var/lib/home-cloud-lab/cpu-gpu-compatibility.json').write_text(json.dumps(r,indent=2)+'\n')
 assert p.returncode==0,(p.stdout,p.stderr)
assert r['release']['restored_drivers']==r['release']['original_drivers']
print(json.dumps({'compute':r['compute'],'host_drivers_restored':True}))
'''
host(['sudo','-n','python3','-c',gpu_code],timeout=450)
for name in ('cpu-isolation','cpu-host-layout','cpu-gpu-compatibility'):
    save(name+'.json',json.loads(host(['sudo','-n','cat','/var/lib/home-cloud-lab/'+name+'.json']).stdout))
subprocess.run([sys.executable,str(ROOT/'lab/verify_client.py')],check=True)
print('CPU isolation, host reboot, GPU compatibility, and Mac access passed',flush=True)
