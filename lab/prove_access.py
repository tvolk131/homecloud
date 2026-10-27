#!/usr/bin/env python3
"""Read-only guest access/fence smoke test for an existing owned lab VM.

No guest credentials or profiles are printed. Exports a private SSH trust/config
file beside the supplied profile, without modifying ~/.ssh/config.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--cli',required=True,type=Path)
    parser.add_argument('--profile',required=True,type=Path)
    parser.add_argument('--vm',required=True)
    parser.add_argument('--ssh-key',type=Path,default=Path.home()/'.ssh/id_ed25519')
    parser.add_argument('--blocked-address',action='append',required=True,help='Host, LAN or other-VM IP to probe on TCP/22')
    args=parser.parse_args()
    cli=[str(args.cli.resolve()),'--profile',str(args.profile.resolve())]
    exported=subprocess.run(cli+['ssh-config',args.vm],capture_output=True,text=True,check=True)
    info=json.loads(exported.stdout)
    ssh=['ssh','-F',info['config'],'-i',str(args.ssh_key),'-o','BatchMode=yes','-o','ConnectTimeout=15']
    script='''import json,socket,urllib.request
targets=json.loads(TARGETS)
assert urllib.request.urlopen('https://example.com',timeout=10).status==200
for address in targets:
    try:
        stream=socket.create_connection((address,22),timeout=2)
    except OSError:continue
    stream.close()
    raise RuntimeError('Forbidden destination reachable: '+address)
print(json.dumps({'public_https':True,'blocked_tcp22':targets}))
'''.replace('TARGETS',repr(json.dumps(args.blocked_address)))
    result=subprocess.run(ssh+[info['alias'],'python3 -'],input=script,capture_output=True,text=True,timeout=20+3*len(args.blocked_address))
    if result.returncode:raise RuntimeError(result.stderr)
    print(result.stdout.strip())
    # A malformed/unknown trust store must not cause a fallback to insecure SSH.
    with tempfile.TemporaryDirectory(prefix='homecloud-wrong-trust-') as directory:
        wrong=Path(directory)/'known_hosts';wrong.write_text('')
        result=subprocess.run(ssh+['-o','UserKnownHostsFile='+str(wrong),info['alias'],'true'],capture_output=True,text=True,timeout=25)
        assert result.returncode!=0 and 'Host key verification failed' in result.stderr
    print(json.dumps({'iroh_ssh':True,'untrusted_host_key_rejected':True}))


if __name__=='__main__':main()
