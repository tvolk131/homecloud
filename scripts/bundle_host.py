#!/usr/bin/python3
"""Package reviewed Linux binaries/helpers, never profiles, credentials or VM data."""
import argparse
from pathlib import Path
import tarfile

ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--bin-dir',type=Path,required=True);p.add_argument('--output',type=Path,required=True);args=p.parse_args()
sources={}
for name in ('homecloudd','homecloud'):sources['bin/'+name]=args.bin_dir/name
for pattern in ('scripts/install_host.py','crates/host/*.py','crates/host/firewall.nft','crates/host/native/*.c','guest/*.py','guest/*.sh'):
    for file in ROOT.glob(pattern):sources[str(file.relative_to(ROOT))]=file
args.output.parent.mkdir(parents=True,exist_ok=True)
with tarfile.open(args.output,'w:gz') as archive:
    for name,source in sorted(sources.items()):
        assert source.is_file(),str(source)
        archive.add(source,arcname=name,recursive=False)
print('Packaged',len(sources),'reviewed files')
