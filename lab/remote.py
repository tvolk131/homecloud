"""Local SSH transport for this explicitly authorized, disposable lab host."""
import json
from pathlib import Path
import shlex
import subprocess

ROOT = Path(__file__).resolve().parent.parent
ACCESS = ROOT / '.host-access'
REMOTE = '/usr/local/libexec/home-cloud-lab'
SSH = ['/usr/bin/ssh', '-F', str(ACCESS / 'ssh_config'), '-o', 'BatchMode=yes',
       '-o', 'ServerAliveInterval=10', '-o', 'ServerAliveCountMax=3', 'hclab-host']


def host(args, input=None, timeout=120, check=True):
    p = subprocess.run(SSH + [shlex.join(args)], input=input, text=True,
                       capture_output=True, timeout=timeout)
    if check and p.returncode:
        raise RuntimeError(f'{args}: {p.stdout}\n{p.stderr}')
    return p


def upload(*names):
    for name in names:
        assert '/' not in name
        code = ('from pathlib import Path;import os,sys;'
                f'p=Path({str(REMOTE + "/" + name)!r});'
                'q=p.with_suffix(p.suffix+".upload");'
                'q.write_text(sys.stdin.read());q.chmod(0o644);os.replace(q,p)')
        host(['sudo', '-n', 'python3', '-c', code], input=(ROOT / 'lab' / name).read_text())


def script(name, *args, input=None, timeout=120):
    return host(['sudo', '-n', 'python3', f'{REMOTE}/{name}', *map(str, args)],
                input=input, timeout=timeout).stdout


def stream_script(name, *args, timeout=120):
    command = ['sudo', '-n', 'python3', f'{REMOTE}/{name}', *map(str, args)]
    subprocess.run(SSH + [shlex.join(command)], check=True, timeout=timeout)


def save(name, data):
    path = ROOT / 'lab/evidence' / name
    path.write_text(json.dumps(data, indent=2) + '\n')
