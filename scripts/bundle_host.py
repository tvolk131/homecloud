#!/usr/bin/python3
"""Package allowlisted software with content hashes; never include device/VM state."""
import argparse
import io
import json
from pathlib import Path
import tarfile
from host_deploy import FORMAT,STATE_FORMAT,SOURCES,digest

ROOT=Path(__file__).resolve().parents[1]

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--bin-dir',type=Path,required=True);p.add_argument('--output',type=Path,required=True);args=p.parse_args()
    sources={name:((args.bin_dir/Path(name).name) if name.startswith('bin/') else ROOT/name) for name in SOURCES}
    files={name:source.read_bytes() for name,source in sources.items()}
    manifest={'format':FORMAT,'state_format':STATE_FORMAT,'architecture':'x86_64','files':{name:{'sha256':digest(data),'bytes':len(data)} for name,data in files.items()}}
    args.output.parent.mkdir(parents=True,exist_ok=True)
    with tarfile.open(args.output,'w:gz') as archive:
        for name,data in sorted({**files,'manifest.json':json.dumps(manifest,sort_keys=True).encode()}.items()):
            entry=tarfile.TarInfo(name);entry.size=len(data);entry.mode=0o755 if name.startswith('bin/') else 0o644
            archive.addfile(entry,io.BytesIO(data))
    print('Packaged',len(files),'reviewed files; SHA-256:',digest(args.output.read_bytes()))

if __name__=='__main__':main()
