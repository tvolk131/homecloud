"""Attribute metadata-only startup events to individual QEMU launch generations."""
import argparse
from collections import Counter
import json
from pathlib import Path
import re


def events(text):
    result = []
    for line in text.splitlines():
        if not line.startswith(('exec ', 'fork ', 'io-enter ', 'io-return ', 'exit ', 'signal ')): continue
        fields = dict(re.findall(r'(\w+)=([^\s]+)', line))
        fields = {k: int(v) if re.fullmatch(r'-?\d+', v) else v for k, v in fields.items()}
        result.append({'kind': line.split(' ', 1)[0], **fields})
    return sorted(result, key=lambda e: e['ns'])


def analyze(text, audit):
    by_pid = {int(e['launcher_pid']): e['vm'] for e in audit}
    roots = {}; family = {}; pending = {}
    for e in events(text):
        pid = e.get('pid'); kind = e['kind']
        if kind == 'exec':
            if e['file'].startswith('/usr/local/libexec/homecloud/qemu/') or pid not in family:
                key = str(pid)+':'+str(e['ns']); family[pid] = key
                roots[key] = {'pid': pid, 'started_ns': e['ns'], 'vm': by_pid.get(pid), 'execs': [], 'forks': [], 'reads': [], 'writes': [], 'exits': []}
            roots[family[pid]]['execs'].append(e)
        elif kind == 'fork':
            key = family.get(e['parent']) or family.get(pid)
            if key:
                family[e['child']] = key; roots[key]['forks'].append(e)
        elif kind == 'io-enter':
            key = family.get(pid) or family.get(e['tid'])
            if key:
                pending[(e['tid'], e['op'].rsplit('_', 1)[1])] = (key, e)
        elif kind == 'io-return':
            previous = pending.pop((e['tid'], e['op'].rsplit('_', 1)[1]), None)
            if previous:
                key, entry = previous
                if entry['fd'] != e['fd']: continue
                item = {**entry, 'return_ns': e['ns'], 'ret': e['ret']}
                roots[key]['reads' if e['op'].endswith('_read') else 'writes'].append(item)
        elif kind == 'exit':
            key = family.get(pid) or family.get(e['tid'])
            if key: roots[key]['exits'].append(e)
    report = []
    for root in roots.values():
        if not root['vm']: continue  # Incus feature probes are separate from VM launches.
        parent_exits = [e for e in root['exits'] if e['pid'] == root['pid'] and e['group_dead']]
        root['parent_raw_status'] = parent_exits[-1]['raw_status'] if parent_exits else None
        root['parent_pipe_reads'] = [e for e in root['reads'] if e['pid'] == root['pid'] and e['tid'] == root['pid'] and e['mode'] & 0o170000 == 0o010000]
        root['paired_pipe_writes'] = [w for w in root['writes'] if any(w['ino'] == r['ino'] for r in root['parent_pipe_reads'])]
        # A successful parent exit proves the source's status-byte check passed;
        # a read returning one alone does not establish the byte's value.
        root['readiness_acknowledged'] = root['parent_raw_status'] == 0 and any(r['ret'] == 1 for r in root['parent_pipe_reads'])
        report.append(root)
    return {'launches': report, 'summary': {'vm_launches': len(report),
            'readiness_acknowledged': sum(r['readiness_acknowledged'] for r in report),
            'matched_pipe_writes': sum(bool(r['paired_pipe_writes']) for r in report),
            'parent_statuses': dict(Counter(str(r['parent_raw_status']) for r in report)),
            'pipe_read_returns': dict(Counter(str(e['ret']) for r in report for e in r['parent_pipe_reads']))}}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--trace', required=True); p.add_argument('--audit', required=True); p.add_argument('--output', required=True)
    a = p.parse_args()
    report = analyze(Path(a.trace).read_text(), [json.loads(l) for l in Path(a.audit).read_text().splitlines() if l])
    Path(a.output).write_text(json.dumps(report, indent=2)+'\n'); print(json.dumps(report['summary']))


if __name__ == '__main__': main()
