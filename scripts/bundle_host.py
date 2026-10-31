#!/usr/bin/python3
"""Package allowlisted software with content hashes; never include device/VM state."""
import argparse
import io
import json
from pathlib import Path
import tarfile
from host_deploy import FORMAT,GUEST_RUNNER,STATE_FORMAT,SOURCES,digest,static_elf

ROOT=Path(__file__).resolve().parents[1]

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--bin-dir',type=Path,required=True);p.add_argument('--output',type=Path,required=True)
    p.add_argument('--guest',type=Path,default=ROOT/'target/x86_64-unknown-linux-musl/release/homecloud-guest',help='Static guest runner')
    args=p.parse_args()
    sources={name:((args.bin_dir/Path(name).name) if name.startswith('bin/') else ROOT/name) for name in SOURCES}
    sources[GUEST_RUNNER]=args.guest
    files={name:source.read_bytes() for name,source in sources.items()}
    if not static_elf(files[GUEST_RUNNER]):raise SystemExit('Build a static guest runner: cargo build --locked --release --target x86_64-unknown-linux-musl -p homecloud-guest')
    manifest={'format':FORMAT,'state_format':STATE_FORMAT,'architecture':'x86_64','files':{name:{'sha256':digest(data),'bytes':len(data)} for name,data in files.items()}}
    args.output.parent.mkdir(parents=True,exist_ok=True)
    with tarfile.open(args.output,'w:gz') as archive:
        for name,data in sorted({**files,'manifest.json':json.dumps(manifest,sort_keys=True).encode()}.items()):
            entry=tarfile.TarInfo(name);entry.size=len(data);entry.mode=0o755 if name.startswith('bin/') else 0o644
            archive.addfile(entry,io.BytesIO(data))
    print('Packaged',len(files),'reviewed files; SHA-256:',digest(args.output.read_bytes()))

if __name__=='__main__':main()
