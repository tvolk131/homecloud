#!/usr/bin/python3
"""Install T3's native provider dependencies as the guest owner, without login.

Pins satisfy T3 0.1.2's bundled compatibility policy. Existing compatible
installations are retained. Downloads are checked before executing/extracting.
The GitHub CLI is an optional developer tool; an outdated system copy (for
example Ubuntu's own package) is shadowed in ~/.local/bin rather than replaced.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

PROVIDERS = {
    'codex': {'version': '0.165.0', 'minimum': (0, 159, 0),
              'url': 'https://github.com/openai/codex/releases/download/rust-v0.165.0/codex-package-x86_64-unknown-linux-musl.tar.gz',
              'sha256': 'e97c4463d7f146692a955d7b0e1c8cf48f97cd4d616cd45c5dcab2e8d9743316'},
    'claude': {'version': '2.1.308', 'minimum': (2, 1, 280),
               'url': 'https://downloads.claude.ai/claude-code-releases/2.1.308/linux-x64/claude',
               'sha256': '356733f185fa988e6bf3aa9a555269fdd167982a2e2a80d3c8a78037bb92ad58'},
    # 2.40 added per-account logout and --insecure-storage, both used by sign-in/out.
    'gh': {'version': '2.103.3', 'minimum': (2, 40, 0), 'shadow_system': True,
           'url': 'https://github.com/cli/cli/releases/download/v2.103.3/gh_2.103.3_linux_amd64.tar.gz',
           'sha256': '4c806978ffce1cec70853302a56d70ac6da4f19bae7adca4e1a8ab00633dd91d'},
}


def version(binary):
    try:
        result = subprocess.run([str(binary), '--version'], capture_output=True, text=True, timeout=30, check=True)
        match = re.search(r'(?<![0-9])(\d+)\.(\d+)\.(\d+)', result.stdout)
        return tuple(map(int, match.groups())) if match else None
    except (OSError, subprocess.SubprocessError): return None


def download(url, path, expected):
    digest = hashlib.sha256()
    request = urllib.request.Request(url, headers={'User-Agent': 'homecloud-guest-installer'})
    with urllib.request.urlopen(request, timeout=60) as response, path.open('wb') as stream:
        while chunk := response.read(1024 * 1024):
            digest.update(chunk); stream.write(chunk)
    if digest.hexdigest() != expected:
        path.unlink()
        raise RuntimeError('Provider download checksum mismatch')


def unpack_codex(archive, destination, expected_version):
    with tarfile.open(archive, 'r:gz') as tar:
        # The official package contains symlinks; Python's data filter prevents
        # escaping the staging directory through paths, links, or special files.
        tar.extractall(destination, filter='data')
    manifest = json.loads((destination/'codex-package.json').read_text())
    if manifest.get('version') != expected_version or manifest.get('target') != 'x86_64-unknown-linux-musl' or manifest.get('entrypoint') != 'bin/codex':
        raise RuntimeError('Unexpected Codex package manifest')
    for relative in ['bin/codex', 'bin/codex-code-mode-host', 'codex-path/rg']:
        path = destination/relative
        if not path.is_file() or not os.access(path, os.X_OK):
            raise RuntimeError('Incomplete Codex native package')


def unpack_gh(archive, destination, expected_version):
    with tarfile.open(archive, 'r:gz') as tar:
        tar.extractall(destination, filter='data')
    root = destination/('gh_'+expected_version+'_linux_amd64')
    binary = root/'bin/gh'
    if not binary.is_file() or binary.is_symlink() or not os.access(binary, os.X_OK):
        raise RuntimeError('Incomplete GitHub CLI package')
    return root


def link(target, destination):
    temporary = destination.with_name(destination.name+'.homecloud-new')
    temporary.unlink(missing_ok=True)
    temporary.symlink_to(target)
    temporary.replace(destination)


def ensure_providers():
    if os.getuid() == 0: raise RuntimeError('Install providers as the guest owner')
    if platform.system() != 'Linux' or platform.machine() != 'x86_64':
        raise RuntimeError('This provider bundle requires Linux x86_64')
    os.umask(0o077)
    bindir = Path.home()/'.local/bin'; bindir.mkdir(parents=True, exist_ok=True)
    for name, policy in PROVIDERS.items():
        try: ensure_provider(name, policy, bindir)
        except (OSError, RuntimeError, ValueError, subprocess.SubprocessError, tarfile.TarError):
            if name != 'gh': raise
            print('gh: optional installation failed; existing tools retained. Repair GitHub CLI separately.', flush=True)


def ensure_provider(name, policy, bindir):
    current = shutil.which(name)
    if current:
        installed = version(current)
        if installed is not None and installed >= policy['minimum']:
            print(name+': compatible installation retained', flush=True); return
        shadow = policy.get('shadow_system') and Path(current).parent != bindir
        if not shadow:
            raise RuntimeError(name+': existing installation is unsupported or broken; update it before repair')
    with tempfile.TemporaryDirectory(prefix='homecloud-provider-') as tmp:
        stage = Path(tmp); artifact = stage/'download'
        print('Installing '+name+' '+policy['version'], flush=True)
        download(policy['url'], artifact, policy['sha256'])
        if name == 'codex':
            root = Path.home()/'.local/share/homecloud/providers/codex'
            root.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix='.stage-', dir=root) as extraction:
                package = Path(extraction)
                unpack_codex(artifact, package, policy['version'])
                target = root/policy['version']
                if target.exists(): shutil.rmtree(target)
                package.rename(target)
            link(target/'bin/codex', bindir/'codex')
            link(target/'bin/codex-code-mode-host', bindir/'codex-code-mode-host')
        elif name == 'gh':
            root = Path.home()/'.local/share/homecloud/providers/gh'
            root.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix='.stage-', dir=root) as extraction:
                package = unpack_gh(artifact, Path(extraction), policy['version'])
                target = root/policy['version']
                if target.exists(): shutil.rmtree(target)
                package.rename(target)
            link(target/'bin/gh', bindir/'gh')
        else:
            artifact.chmod(0o700)
            subprocess.run([str(artifact), 'install', policy['version']], check=True, timeout=300,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        installed = version(bindir/name)
        if installed != tuple(map(int, policy['version'].split('.'))):
            raise RuntimeError(name+': installed version does not match requested release')
        print(name+': installed and verified', flush=True)

if __name__ == '__main__': ensure_providers()
