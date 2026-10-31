"""Cross-language test entrypoint: real adapter dispatch, fake Incus/OS only."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch

BOOT = '00000000-0000-4000-8000-000000000010'
VM_ID = '00000000-0000-4000-8000-000000000001'
OWNER = '00000000-0000-4000-8000-000000000002'
NAME = 'hc-' + VM_ID.replace('-', '')
spec = importlib.util.spec_from_file_location('shutdown_bridge_host', Path(__file__).parents[1] / 'host_runtime.py')
host = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = host
spec.loader.exec_module(host)


def api(method, path, data=None, missing=False):
    assert method == 'GET'
    if path == '/1.0/instances?recursion=1&all-projects=true':
        return [{'name': NAME, 'status': 'Stopped', 'project': 'default'}]
    if path == '/1.0/instances/' + NAME:
        return {'config': {'user.homecloud.id': VM_ID, 'user.homecloud.owner': OWNER}}
    if path == '/1.0/instances/' + NAME + '/state':
        return {'status': 'Stopped'}
    raise AssertionError('Unexpected fake Incus query: ' + path)


def run(*args, **kwargs):
    # Never execute an OS command from this test entrypoint.
    assert args == ('systemctl', 'poweroff') and kwargs == {'timeout': 15}
    return subprocess.CompletedProcess(args, 0, '', '')


with tempfile.TemporaryDirectory() as state:
    with patch.object(host.os, 'geteuid', return_value=0), \
         patch.object(host, 'STATE', Path(state)), \
         patch.object(host, 'load_config', return_value={}), \
         patch.object(host, 'shutdown_boot', return_value=BOOT), \
         patch.object(host, 'api', side_effect=api), \
         patch.object(host, 'run', side_effect=run), \
         patch.object(host.instance_runtime, 'live_processes', return_value=[]):
        try:
            reply = host.main()
        except Exception as error:
            reply = {'kind': 'error', 'data': str(error)}
        print(json.dumps(reply))
