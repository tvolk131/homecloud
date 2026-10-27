"""Repeat the lab proofs on the pinned, explicitly disposable test host.

Intentionally creates/deletes tagged scratch VMs/pools and reboots the host.
The component stages have been exercised individually; this assembles them.
Never point these hardware-specific scripts at a production or unknown host.
"""
import json
import subprocess
import sys
from remote import ROOT, host, save, stream_script, upload

upload('configure_cpu.py')
active=host(['sudo','-n','test','-f','/etc/systemd/system/incus.service.d/home-cloud-lab-cpu.conf'],check=False).returncode==0
if active:
    print('Restoring the previous static CPU prototype before creating fixtures',flush=True)
    stream_script('configure_cpu.py','restore',timeout=600)
# Retain each original configuration as evidence; allow a fresh installation.
code='from pathlib import Path;import time;p=Path("/var/lib/home-cloud-lab/cpu-before.json");p.rename(p.with_name("cpu-before-restored-"+str(time.time_ns())+".json")) if p.exists() else None'
host(['sudo','-n','python3','-c',code])
subprocess.run([sys.executable,str(ROOT/'lab/prove_provisioning.py')],check=True)
upload('prove_storage.py')
stream_script('prove_storage.py',timeout=4000)
save('storage-guarantees.json',json.loads(host(['sudo','-n','cat','/var/lib/home-cloud-lab/storage-guarantees.json']).stdout))
subprocess.run([sys.executable,str(ROOT/'lab/prove_cpu_from_mac.py')],check=True)
upload('capture_current_state.py')
stream_script('capture_current_state.py')
subprocess.run([sys.executable,str(ROOT/'lab/fetch_evidence.py')],check=True)
print('Component proof suite complete; static CPU policy is active for the two lab VMs',flush=True)
