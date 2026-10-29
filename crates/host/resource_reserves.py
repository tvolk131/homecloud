"""Fixed-path, recoverable host reserve policy; never runs client-supplied code."""
import copy
import json
import os
from pathlib import Path
import uuid
import arc_policy
import memory_capacity

GIB = 1024**3
KEYS = ('memory_bytes', 'cpu_cores', 'disk_bytes')


def initial():
    return {'state': {'revision': 0, 'additional': dict.fromkeys(KEYS, 0), 'core_groups': [], 'arc': None},
            'phase': 'applied', 'operation_id': None, 'discarded': []}


def validate(base, state):
    assert type(state['revision']) is int and 0 <= state['revision'] < 2**64
    extra = state['additional']
    assert set(extra) == set(KEYS)
    assert all(type(extra[k]) is int and 0 <= extra[k] <= base['capacity'][k] for k in KEYS), 'Reserve exceeds reviewed capacity'
    assert extra['memory_bytes'] % GIB == 0 and extra['disk_bytes'] % GIB == 0, 'Use whole GiB for reserves'
    groups = state['core_groups']
    assert len(groups) == extra['cpu_cores'] and all(g in base['cpu_groups'] for g in groups), 'Reserve splits or invents physical cores'
    flattened = [cpu for group in groups for cpu in group]
    assert len(flattened) == len(set(flattened)), 'Duplicate reserved core'
    assert base['minimum_pool_available_bytes'] + extra['disk_bytes'] < 2**64
    if base.get('minimum_reserves'):
        minimum=base['minimum_reserves']
        groups=set()
        house=set(base['housekeeping'])
        for cpu in house:
            siblings=Path(f'/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list').read_text().strip()
            members=set()
            for part in siblings.split(','):
                ends=[int(v) for v in part.split('-')];members.update(range(ends[0],ends[-1]+1))
            assert members<=house,'Housekeeping splits a physical core'
            groups.add(tuple(sorted(members)))
        assert minimum['cpu_cores']==len(groups),'CPU minimum differs from complete housekeeping cores'
        assert minimum['disk_bytes']==base['minimum_pool_available_bytes'],'Pool floor minimum differs from host policy'


def read_record(host, base):
    path = host.STATE / 'resource-reserves.json'
    if not path.exists():
        return initial()
    st = path.stat()
    assert st.st_uid == 0 and not st.st_mode & 0o077, 'Reserve journal must be private and root-owned'
    record = json.loads(path.read_text())
    assert record['phase'] in ('applying', 'applied')
    record['state'].setdefault('arc',None)
    if record['operation_id'] is None:
        assert record['state'] == initial()['state']
    else:
        uuid.UUID(record['operation_id'])
    for op_id in record.get('discarded', []):
        uuid.UUID(op_id)
    validate(base, record['state'])
    return record


def write_record(host, record):
    host.save(host.STATE / 'resource-reserves.json', record)
    fd = os.open(host.STATE, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def effective(base, record):
    validate(base, record['state'])
    state = record['state']
    result = copy.deepcopy(base)
    result['capacity'] = {k: base['capacity'][k] - state['additional'][k] for k in KEYS}
    result['cpu_groups'] = [g for g in base['cpu_groups'] if g not in state['core_groups']]
    result['housekeeping'] += [cpu for group in state['core_groups'] for cpu in group]
    result['minimum_pool_available_bytes'] += state['additional']['disk_bytes']
    result['_reserve_base'] = base
    result['_reserve_record'] = record
    return result


def report(base, record):
    return {'excluded_network_slots':base.get('excluded_network_slots',[]),'memory':memory_capacity.status(effective(base,record)), 'envelope': base['capacity'], 'core_groups': base['cpu_groups'],
            'disk_overhead_gib_per_vm': base.get('disk_overhead_gib_per_vm', 2),
            'base_pool_free_bytes': base['minimum_pool_available_bytes'],
            'state': record['state'], 'applying': record['phase'] != 'applied',
            'operation_id': record['operation_id'],
            'minimum_reserves':base.get('minimum_reserves',dict.fromkeys(KEYS,0)),
            'arc':arc_policy.observe(), 'memory_policy':arc_policy.policy(base)}


def enforce(host, config, vms, activate=False):
    house = host.cpulist(config['housekeeping'])
    # Separate managed drop-ins sort after the install-time configuration.
    # The root-owned journal is also replayed by cpu-layout before Incus at boot.
    for unit in ('system.slice', 'user.slice'):
        host.text(Path('/etc/systemd/system') / (unit + '.d') / 'zz-homecloud-reserves.conf',
                  '[Slice]\nAllowedCPUs=\nAllowedCPUs=' + house + '\n')
    host.text(Path('/etc/systemd/system/incus.service.d/zz-homecloud-reserves.conf'),
              '[Service]\nCPUAffinity=\nCPUAffinity=' + house.replace(',', ' ') + '\n')
    host.run('systemctl', 'daemon-reload')
    for unit in ('system.slice', 'user.slice'):
        host.run('systemctl', 'set-property', '--runtime', unit, 'AllowedCPUs=' + house)
    host.cpu_layout(config)
    host.cpu_policy(config, vms, activate=activate)
    verify(host, config, vms)


def verify(host, config, vms):
    house = set(config['housekeeping'])
    assert os.sched_getaffinity(1) == house, 'Service manager CPU affinity is not enforced'
    path = Path('/sys/fs/cgroup/system.slice/cpuset.cpus.effective')
    assert path.exists() and host.parse_cpus(path.read_text()) == house, 'Host process CPU fence is not enforced'
    user = Path('/sys/fs/cgroup/user.slice/cpuset.cpus.effective')
    if user.exists():
        assert host.parse_cpus(user.read_text()) == house, 'User process CPU fence is not enforced'
    entries = [dict(entry, must_exist=True) for entry in config.get('legacy_vms', [])]
    for vm in vms:
        if vm['cpu_ids'] and vm['observed'] != 'deleted':
            entries.append({'unit': 'homecloud-' + uuid.UUID(vm['id']).hex + '.service', 'cpu_ids': vm['cpu_ids'], 'must_exist': vm['observed'] == 'running'})
    for entry in entries:
        cpus = Path('/sys/fs/cgroup/homecloud.slice') / entry['unit'] / 'cpuset.cpus.effective'
        if not cpus.exists():
            assert not entry['must_exist'], 'Running guest CPU anchor is missing; repair it without restarting the VM'
        else:
            assert host.parse_cpus(cpus.read_text()) == house | set(entry['cpu_ids']), 'Guest process CPU fence is not enforced'


def apply(host, config, operation, vms, users):
    base = config['_reserve_base']
    record = config['_reserve_record']
    before, target = operation['before'], operation['target']
    op_id = str(uuid.UUID(operation['id']))
    assert op_id not in record.get('discarded', []), 'Reserve operation was discarded on the host'
    validate(base, before)
    validate(base, target)
    assert operation['phase'] == 'applying'
    assert target['revision'] == before['revision'] + 1, 'Reserve revision is stale'
    assert (record['state'] == before and record['phase'] == 'applied') or (
        record['state'] == target and record['operation_id'] == op_id), 'Unexpected host reserve journal; review required'
    if record['state'] == target and record['phase'] == 'applied':
        # A delayed duplicate must never restore an old VM/CPU snapshot.
        current = json.loads((host.STATE / 'cpu-vms.json').read_text()) if (host.STATE / 'cpu-vms.json').exists() else []
        verify(host, config, current)
        assert arc_policy.enforced(target.get('arc'),config),'ARC policy drift needs inspection'
        return report(base, record)
    persist_arc = (record.get('arc_persistence_required', True) if record['state'] == target else
                   before.get('arc') != target.get('arc') or not arc_policy.enforced(target.get('arc'),effective(base,{'state':target})))
    next_record = {'state': target, 'operation_id': op_id, 'phase': 'applying',
                   'discarded': record.get('discarded', []), 'arc_persistence_required':persist_arc}
    proposed = effective(base, next_record)
    arc_policy.validate(proposed,target.get('arc'))
    assert target.get('arc') is not None or arc_policy.enforced(None,proposed), 'Choose Automatic or a custom ARC limit to repair an unmanaged cache policy'
    arc_policy.restore_reclaim(record,lambda v:write_record(host,v))
    for key in KEYS:
        promised = sum(max(u['grant'][key], u['committed'][key]) for u in users)
        old_capacity=config['capacity'][key];new_capacity=proposed['capacity'][key]
        if key=='memory_bytes' and memory_capacity.status(proposed) is not None:
            safe=memory_capacity.status(proposed)['safe_bytes']
            old_capacity=max(0,min(base['capacity'][key],safe)-before['additional'][key])
            new_capacity=max(0,min(base['capacity'][key],safe)-target['additional'][key])
        assert new_capacity>=old_capacity or promised <= new_capacity, 'Reserve would consume capacity promised to users'
    reserved = {cpu for group in target['core_groups'] for cpu in group}
    assert all(not reserved.intersection(vm['cpu_ids']) for vm in vms if vm.get('observed') != 'deleted'), 'Reserve would take a claimed physical core'
    old_path = host.STATE / 'cpu-vms.json'
    if old_path.exists():
        old = json.loads(old_path.read_text())
        assert all(not reserved.intersection(vm['cpu_ids']) for vm in old if vm.get('observed') != 'deleted'), 'Host still holds a CPU claim; finish cleanup first'
    # Bring unused grant backing in line with the database before assessing the
    # new floor. This never drops below any VM commitment. A lower target floor
    # must also remain usable as a recovery from exhausted old headroom.
    syncing = copy.deepcopy(config)
    syncing['minimum_pool_available_bytes'] = min(config['minimum_pool_available_bytes'], proposed['minimum_pool_available_bytes'])
    host.backing(syncing, users)
    assert host.zget(base['pool'], 'available') >= proposed['minimum_pool_available_bytes'], 'Insufficient physical pool headroom for this reserve'
    # Persist target before side effects. An interrupted apply remains explicitly
    # unconfirmed, holds conservative DB fences, and is replayed at boot/retry.
    write_record(host, next_record)
    enforce(host, proposed, vms)
    arc_policy.apply(host,proposed,target.get('arc'),persist=persist_arc)
    next_record['arc_persistence_required']=False
    write_record(host,next_record)
    if target['additional']['memory_bytes']<before['additional']['memory_bytes']:
        arc_policy.reclaim_for_release(proposed,next_record,lambda v:write_record(host,v))
    next_record['phase'] = 'applied'
    write_record(host, next_record)
    return report(base, next_record)


def boot(host, config, vms):
    record = config['_reserve_record']
    if record['operation_id'] is None:
        host.cpu_layout(config)
        host.cpu_policy(config, vms)
        return
    arc_policy.restore_reclaim(record,lambda v:write_record(host,v))
    enforce(host, config, vms, activate=True)
    # A reboot may interrupt persistence itself. Finish that rebuild before
    # confirming the journal; a live cap and config file alone are insufficient.
    persist_arc = record['phase'] != 'applied' and record.get('arc_persistence_required', True)
    try:
        arc_policy.apply(host,config,record['state'].get('arc'),persist=persist_arc)
        arc_policy.restore_reclaim(record,lambda v:write_record(host,v))
        arc_policy.reclaim_for_release(config,record,lambda v:write_record(host,v))
    except Exception as error:
        # No managed guest has Incus autostart enabled. Keep the management and
        # recovery APIs reachable; running_guard rejects workload starts.
        record['cache_error']=str(error);write_record(host,record)
        return
    record.pop('cache_error',None)
    record['arc_persistence_required']=False
    record['phase'] = 'applied'
    write_record(host, record)


def discard(host, config, operation):
    record = copy.deepcopy(config['_reserve_record'])
    op_id = str(uuid.UUID(operation['id']))
    assert record['state'] == operation['before'] and record['phase'] == 'applied', 'Reserve effects have begun; retry recovery instead'
    # A durable tombstone also fences a timed-out/orphaned apply process that is
    # still queued for the host lock. A status observation alone cannot do that.
    ids = record.setdefault('discarded', [])
    if op_id not in ids:
        ids.append(op_id)
        write_record(host, record)
    return {'operation_id': op_id, 'policy': report(config['_reserve_base'], record)}
