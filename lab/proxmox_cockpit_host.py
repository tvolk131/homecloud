"""Pinned-SSH helper for the disposable cockpit experiment, not a host installer."""

import base64
import hashlib
import http.client
import io
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import ssl
import subprocess
import sys
import tarfile


def run(*arguments, timeout=120, check=True, **options):
    result = subprocess.run(arguments, text=True, capture_output=True, timeout=timeout, **options)
    if check and result.returncode:
        raise RuntimeError(' '.join(arguments[:3]) + ': ' + (result.stderr or result.stdout)[-2500:])
    return result


def identities(state):
    token = state['run_id']
    if len(token) != 32 or any(character not in '0123456789abcdef' for character in token):
        raise RuntimeError('Invalid run identity')
    expected = {'instance': 'hc-pve-' + token[:10], 'storage': 'hcpve-' + token[:10],
                'network': 'hcvmsp' + token[:8], 'dataset': 'hclab/proxmox-spike-' + token}
    if any(state.get(key) != value for key, value in expected.items()):
        raise RuntimeError('Resource identity mismatch')
    return token, Path('/tmp') / ('homecloud-cockpit-' + token)


def baseline():
    instances = json.loads(run('incus', 'list', '--format=json').stdout)
    networks = json.loads(run('incus', 'network', 'list', '--format=json').stdout)
    if any(entry.get('config', {}).get('ipv4.address') == '172.31.246.1/24' for entry in networks):
        raise RuntimeError('Disposable subnet is already occupied')
    cpu_policy = Path('/etc/homecloud/cpu-policy.tsv').read_bytes()
    runtime_files = ['/usr/lib/x86_64-linux-gnu/libssl.so.3', '/usr/lib/x86_64-linux-gnu/libcrypto.so.3']
    return {
        'os': run('head', '-5', '/etc/os-release').stdout.strip(),
        'controller': run('systemctl', 'is-active', 'homecloudd', check=False).stdout.strip(),
        'cpu_policy_sha256': hashlib.sha256(cpu_policy).hexdigest(),
        'cpu_policy_mode': Path('/etc/homecloud/cpu-policy.tsv').stat().st_mode & 0o777,
        'physical_package_database_sha256': hashlib.sha256(Path('/var/lib/dpkg/status').read_bytes()).hexdigest(),
        'physical_ca_bundle_sha256': hashlib.sha256(Path('/etc/ssl/certs/ca-certificates.crt').read_bytes()).hexdigest(),
        'root_rustup_exists': Path('/root/.rustup').exists(),
        'openssl_runtime': {name: {
            'uid': Path(name).stat().st_uid, 'gid': Path(name).stat().st_gid,
            'mode': Path(name).stat().st_mode & 0o777,
            'sha256': hashlib.sha256(Path(name).read_bytes()).hexdigest(),
        } for name in runtime_files},
        'instances': {entry['name']: {key: entry[key] for key in ('status', 'config', 'devices', 'profiles')}
                      for entry in instances},
        'storage_names': sorted(entry['name'] for entry in json.loads(run('incus', 'storage', 'list', '--format=json').stdout)),
        'network_names': sorted(entry['name'] for entry in networks),
        'datasets': sorted(run('zfs', 'list', '-H', '-o', 'name', '-r', 'hclab').stdout.splitlines()),
        'pool_health': run('zpool', 'list', '-H', '-o', 'health', 'hclab').stdout.strip(),
        'parent_pool_properties': run('zfs', 'get', '-Hp', '-o', 'property,value', 'quota,reservation,mountpoint', 'hclab').stdout.strip(),
    }


def owned(directory, token):
    if directory.is_symlink() or not directory.is_dir() or (directory / 'run-id').is_symlink():
        raise RuntimeError('Missing or unsafe build directory')
    if (directory / 'run-id').read_text() != token:
        raise RuntimeError('Foreign build directory')


def compare_baseline(before, after, images):
    differences = [key for key in before if before[key] != after[key]]
    added = sorted(set(after['datasets']) - set(before['datasets']))
    removed = sorted(set(before['datasets']) - set(after['datasets']))
    pattern = re.compile(r'.*/incus/(?:deleted/)?images/([0-9a-f]{64})(?:\.block)?$')
    changed = [pattern.fullmatch(name) for name in added + removed]
    auto_updated = {entry['fingerprint'] for entry in images if entry.get('auto_update')}
    cache_only = bool(changed) and all(changed) and any(match.group(1) in auto_updated for match in changed)
    unexpected = [key for key in differences if key != 'datasets' or not cache_only]
    return {
        'unexpected_fields': unexpected, 'baseline_exactly_restored': not differences,
        'baseline_configuration_restored': not unexpected,
        'unrelated_image_cache_churn': {'added': added, 'removed': removed} if cache_only else None,
    }


def worker(state):
    value = json.loads(run('incus', 'query', '/1.0/instances/' + state['instance']).stdout)
    if value['config'].get('user.homecloud.spike') != state['run_id']:
        raise RuntimeError('Foreign worker')


def guest(state, *arguments):
    worker(state)
    return run('incus', 'exec', state['instance'], '--', *arguments).stdout.strip()


def build(request, directory, token):
    directory.mkdir(mode=0o700)
    (directory / 'run-id').write_text(token)
    account = pwd.getpwnam('tommy')
    rust_environment = dict(os.environ, HOME=account.pw_dir, RUSTUP_HOME=account.pw_dir + '/.rustup')
    rust_version = run(account.pw_dir + '/.cargo/bin/rustc', '--version', env=rust_environment).stdout.strip()
    source = directory / 'source'
    source.mkdir(mode=0o700)
    with tarfile.open(fileobj=io.BytesIO(base64.b64decode(request['archive'])), mode='r:gz') as archive:
        archive.extractall(source, filter='data')
    probe = source / 'crates/cockpit-live-probe'
    (probe / 'src').mkdir(parents=True)
    (probe / 'src/main.rs').write_text(request['probe'])
    (probe / 'Cargo.toml').write_text('''[package]
name = "homecloud-cockpit-live-probe"
version.workspace = true
edition.workspace = true
publish.workspace = true
[dependencies]
anyhow.workspace = true
hex.workspace = true
homecloud-client.workspace = true
homecloud-core.workspace = true
homecloud-daemon = { path = "../daemon", features = ["proxmox"] }
homecloud-store.workspace = true
homecloud-virtualization = { workspace = true, features = ["proxmox"] }
iroh.workspace = true
serde_json.workspace = true
tempfile.workspace = true
tokio.workspace = true
''')
    cargo_home = directory / 'cargo-home'
    cargo_home.mkdir()
    for name in ('registry', 'git'):
        shutil.copytree(Path(account.pw_dir) / '.cargo' / name, cargo_home / name)
    development = directory / 'openssl'
    development.mkdir()
    run('apt-get', 'download', 'libssl-dev', cwd=development, timeout=180)
    packages = list(development.glob('libssl-dev_*.deb'))
    if len(packages) != 1:
        raise RuntimeError('Ambiguous private OpenSSL package')
    run('dpkg-deb', '-x', str(packages[0]), str(development))
    headers = development / 'usr/include/openssl'
    for header in (development / 'usr/include/x86_64-linux-gnu/openssl').glob('*.h'):
        if not (headers / header.name).exists():
            (headers / header.name).symlink_to(header)
    libraries = development / 'lib'
    libraries.mkdir()
    for name in ('ssl', 'crypto'):
        (libraries / ('lib' + name + '.so')).symlink_to('/usr/lib/x86_64-linux-gnu/lib' + name + '.so.3')
    run('chown', '-hR', str(account.pw_uid) + ':' + str(account.pw_gid), str(directory))
    housekeeping = json.loads(Path('/etc/homecloud/host.json').read_text())['housekeeping']
    environment = [
        'HOME=' + account.pw_dir, 'RUSTUP_HOME=' + account.pw_dir + '/.rustup',
        'CARGO_HOME=' + str(cargo_home), 'CARGO_TARGET_DIR=' + str(directory / 'target'),
        'PATH=' + account.pw_dir + '/.cargo/bin:/usr/bin:/bin', 'CARGO_BUILD_JOBS=3',
        'OPENSSL_INCLUDE_DIR=' + str(development / 'usr/include'),
        'OPENSSL_LIB_DIR=' + str(libraries), 'OPENSSL_STATIC=0', 'OPENSSL_NO_PKG_CONFIG=1',
    ]
    commands = (
        'cargo test --offline -p homecloud-virtualization --features proxmox --lib && '
        'cargo build --offline -p homecloud-cockpit-live-probe -p homecloud-daemon && '
        'cargo tree --offline -p homecloud-cockpit-live-probe -i proxmox-client'
    )
    with (directory / 'build.log').open('w') as output:
        completed = subprocess.run([
            'systemd-run', '--quiet', '--wait', '--pipe', '--collect', '--uid=tommy',
            '--unit=homecloud-cockpit-build-' + token, '--working-directory=' + str(source),
            '--property=AllowedCPUs=' + ','.join(map(str, housekeeping)),
            '--property=MemoryMax=6G', '--property=TasksMax=512',
            '/usr/bin/env', *environment, '/bin/sh', '-c', commands,
        ], text=True, stdout=output, stderr=subprocess.STDOUT, timeout=2100)
    log = (directory / 'build.log').read_text()
    if completed.returncode:
        raise RuntimeError('Isolated build failed: ' + log[-3500:])
    return {'rust': rust_version,
            'tests_and_build': log[-5500:], 'host_packages_installed': False,
            'test_output': [line for line in log.splitlines() if line.startswith(('test ', 'running '))],
            'compiled_commands': commands, 'probe_source_sha256': hashlib.sha256(request['probe'].encode()).hexdigest(),
            'private_cargo_home': True, 'private_openssl_deb': packages[0].name}


def validate(state, directory):
    private = directory / 'validation'
    private.mkdir(mode=0o700)
    account = pwd.getpwnam('tommy')
    os.chown(private, account.pw_uid, account.pw_gid)
    credentials = {name: json.loads(guest(state, 'cat', '/root/homecloud-proxmox-spike/' + name + '-token.json'))
                   for name in ('reader', 'noaccess')}
    fingerprint = guest(state, 'cat', '/root/homecloud-proxmox-spike/tls-fingerprint')
    (private / 'ca.pem').write_text(guest(state, 'cat', '/etc/pve/pve-root-ca.pem'))
    context = ssl.create_default_context(cafile=str(private / 'ca.pem'))
    context.check_hostname = False
    connection = http.client.HTTPSConnection('172.31.246.2', 8006, context=context, timeout=30)
    connection.connect()
    if hashlib.sha256(connection.sock.getpeercert(binary_form=True)).hexdigest() != fingerprint:
        raise RuntimeError('Reference HTTPS certificate differs from independent guest certificate')
    credential = credentials['reader']
    connection.request('GET', '/api2/extjs/cluster/resources', headers={
        'Authorization': 'PVEAPIToken=' + credential['full-tokenid'] + '=' + credential['value'],
    })
    response = connection.getresponse()
    if response.status != 200:
        raise RuntimeError('Reference read-only request failed')
    reference = json.loads(response.read())['data']
    connection.close()
    binary = directory / 'target/debug/homecloud-cockpit-live-probe'

    def probe(mode, filename, environment=None):
        return json.loads(run('runuser', '-u', 'tommy', '--', str(binary), mode, str(filename),
                              timeout=90, env=environment).stdout)

    results = {}
    for name in ('reader', 'trusted-ca', 'trusted-ca-bad-fingerprint', 'noaccess', 'invalid',
                 'bad-fingerprint', 'untrusted-ca', 'public-secret'):
        credential = credentials['noaccess' if name == 'noaccess' else 'reader']
        secret = private / (name + '-secret')
        secret.write_text('invalid-disposable-token' if name == 'invalid' else credential['value'])
        secret.chmod(0o644 if name == 'public-secret' else 0o600)
        os.chown(secret, account.pw_uid, account.pw_gid)
        config = {
            'endpoint': 'https://172.31.246.2:8006/', 'token_id': credential['full-tokenid'],
            'token_secret_file': str(secret), 'tls_fingerprint': fingerprint,
        }
        if name in ('bad-fingerprint', 'trusted-ca-bad-fingerprint'):
            config['tls_fingerprint'] = '00' * 32
        if name in ('trusted-ca', 'untrusted-ca'):
            config['tls_fingerprint'] = None
        filename = private / (name + '.json')
        filename.write_text(json.dumps(config))
        filename.chmod(0o600)
        os.chown(filename, account.pw_uid, account.pw_gid)
        environment = dict(os.environ)
        environment.pop('SSL_CERT_FILE', None)
        environment.pop('SSL_CERT_DIR', None)
        if name in ('trusted-ca', 'trusted-ca-bad-fingerprint'):
            environment['SSL_CERT_FILE'] = str(private / 'ca.pem')
        results[name] = probe('direct', filename, environment)
    results['iroh'] = probe('iroh', private / 'reader.json')
    environment = dict(os.environ, SSL_CERT_FILE=str(private / 'ca.pem'))
    environment.pop('SSL_CERT_DIR', None)
    results['iroh-trusted-ca'] = probe('iroh', private / 'trusted-ca.json', environment)
    serialized = json.dumps(results)
    if any(credential['value'] in serialized for credential in credentials.values()):
        raise RuntimeError('Secret leaked in probe output')
    return {'reference_resources': reference, 'cases': results, 'token_values_redacted': True,
            'probe_user': account.pw_name, 'probe_uid': int(run('runuser', '-u', 'tommy', '--', 'id', '-u').stdout),
            'pve_services': guest(state, 'systemctl', 'is-active', 'pveproxy', 'pvedaemon', 'pvestatd')}


def main():
    request = json.load(sys.stdin)
    state = request['state']
    token, directory = identities(state)
    action = request['action']
    if action == 'baseline':
        value = baseline()
        if value['pool_health'] != 'ONLINE':
            raise RuntimeError('Physical pool is not healthy')
    elif action == 'build':
        value = build(request, directory, token)
    elif action == 'validate':
        owned(directory, token)
        value = validate(state, directory)
    elif action == 'stop-build':
        if directory.exists():
            owned(directory, token)
        run('systemctl', 'stop', 'homecloud-cockpit-build-' + token + '.service', check=False)
        value = {'build_stopped': True}
    elif action == 'verify-cleanup':
        after = baseline()
        images = json.loads(run('incus', 'image', 'list', '--format=json').stdout)
        comparison = compare_baseline(state['baseline'], after, images)
        if comparison['unexpected_fields']:
            raise RuntimeError('Host baseline differs after cleanup: ' + ', '.join(comparison['unexpected_fields']))
        manifest = Path('/var/lib/homecloud/test-runs') / ('proxmox-' + token)
        if manifest.exists():
            if manifest.is_symlink() or json.loads((manifest / 'manifest.json').read_text())['run_id'] != token:
                raise RuntimeError('Foreign host manifest')
            shutil.rmtree(manifest)
        if directory.exists():
            owned(directory, token)
            shutil.rmtree(directory)
        unit_states = {}
        for unit in ('homecloud-' + token, 'homecloud-cockpit-build-' + token):
            if run('systemctl', 'is-active', unit + '.service', check=False).returncode == 0:
                raise RuntimeError('Owned transient unit remains active')
            unit_states[unit] = run('systemctl', 'show', '--property=LoadState', '--value',
                                    unit + '.service', check=False).stdout.strip()
            if unit_states[unit] != 'not-found':
                raise RuntimeError('Owned transient unit remains loaded')
        value = {**comparison, 'original_workloads_unchanged': True,
                 'build_directory_absent': not directory.exists(), 'host_manifest_absent': not manifest.exists(),
                 'transient_units_absent': True, 'unit_load_states': unit_states,
                 'baseline_after': after}
    else:
        raise RuntimeError('Unknown host helper action')
    print(json.dumps(value))


if __name__ == '__main__':
    main()
