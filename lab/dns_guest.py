"""Disposable guest only: exercise the shipped APT retry policy with real DNS faults."""
from pathlib import Path
import json,os,socket,subprocess,tempfile,threading,time,uuid
assert os.geteuid()==0 and Path('/etc/homecloud-recovery-proof').is_file()
assert subprocess.run(['systemd-detect-virt','--vm','--quiet']).returncode==0,'Refusing to run this fault fixture on a physical host'
resolver=Path('/etc/resolv.conf');old_link=os.readlink(resolver) if resolver.is_symlink() else None;old=resolver.read_bytes()
upstream_dns=next(l.split()[1] for l in Path('/run/systemd/resolve/resolv.conf').read_text().splitlines() if l.startswith('nameserver ') and not l.split()[1].startswith('127.'))
results=[]
with tempfile.TemporaryDirectory(prefix='hc-dns-proof-') as root:
 root=Path(root);root.chmod(0o755)
 subprocess.run(['apt-cache','show','hello'],check=True,capture_output=True)
 for persistent in (False,True):
  stop=threading.Event();count={'failed':0,'forwarded':0};server=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);server.bind(('127.0.0.1',53));server.settimeout(.2)
  def serve():
   while not stop.is_set():
    try:data,peer=server.recvfrom(4096)
    except socket.timeout:continue
    if persistent or count['failed']<2:
     count['failed']+=1;reply=data[:2]+b'\x81\x82'+data[4:6]+b'\x00\x00\x00\x00\x00\x00'+data[12:]
    else:
     count['forwarded']+=1
     with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as upstream:
      upstream.settimeout(3);upstream.sendto(data,(upstream_dns,53));reply=upstream.recv(4096)
    server.sendto(reply,peer)
  worker=threading.Thread(target=serve,daemon=True);worker.start()
  downloads=root/('persistent' if persistent else 'transient');downloads.mkdir();downloads.chmod(0o777)
  try:
   resolver.unlink();resolver.write_text('nameserver 127.0.0.1\noptions timeout:1 attempts:1\n')
   start=time.monotonic()
   p=subprocess.run(['apt-get','-o','Acquire::Retries=3','-o','Acquire::ForceIPv4=true','download','hello'],cwd=downloads,capture_output=True,text=True,timeout=70)
   result={'persistent':persistent,'exit':p.returncode,'seconds':round(time.monotonic()-start,2),**count}
   assert count['failed']>=2
   if persistent:assert p.returncode!=0 and count['forwarded']==0
   else:assert p.returncode==0 and count['forwarded']>0,json.dumps({'stderr':p.stderr,'stdout':p.stdout,'counts':count})
   results.append(result)
  finally:
   resolver.unlink(missing_ok=True)
   if old_link is not None:resolver.symlink_to(old_link)
   else:resolver.write_bytes(old)
   stop.set();worker.join(timeout=4);server.close()
assert resolver.read_bytes()==old
print(json.dumps({'dns_faults':results,'resolver_restored':True}))
