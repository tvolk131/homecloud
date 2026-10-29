"""Observed physical RAM and root-owned reviewed envelope; never drops promises."""
import copy
import json
import time
import uuid
import arc_policy

GIB=1024**3


def record(host,base):
    path=host.STATE/'memory-capacity.json'
    if not path.exists():return {'revision':0,'bytes':base['capacity']['memory_bytes'],'operations':{}}
    st=path.stat();assert st.st_uid==0 and not st.st_mode&0o077,'Memory capacity journal must be private and root-owned'
    r=json.loads(path.read_text());assert type(r['revision']) is int and r['revision']>=0
    assert type(r['bytes']) is int and r['bytes']>=0
    return r


def overlay(host,base):
    result=copy.deepcopy(base);r=record(host,base)
    result['capacity']['memory_bytes']=r['bytes'];result['_memory_capacity']=r
    return result


def status(config):
    base=config.get('_reserve_base',config)
    if arc_policy.policy(base) is None:return None
    r=base.get('_memory_capacity',{'revision':0,'bytes':base['capacity']['memory_bytes']})
    physical=arc_policy.physical_memory();minimum=base.get('minimum_reserves',{}).get('memory_bytes',0)
    safe=max(0,physical-minimum)//GIB*GIB
    ready=False;error=None
    try:ready=arc_policy.enforced(config.get('_reserve_record',{}).get('state',{}).get('arc'),config)
    except Exception as e:error=str(e)
    if not ready:error=error or 'ARC policy is not enforced; VM starts are blocked. Repair cache policy before retrying.'
    return {'revision':r['revision'],'reviewed_bytes':r['bytes'],'safe_bytes':safe,'physical_bytes':physical,
            'minimum_system_bytes':minimum,'boot_id':__import__('pathlib').Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
            'observed_at':int(time.time()),'arc_ready':ready,'error':error}


def adopt(host,config,operation):
    base=config['_reserve_base'];r=record(host,base);op_id=str(uuid.UUID(operation['id']))
    target=operation['target_bytes'];expected=operation['expected_revision']
    if op_id in r['operations']:
        previous=r['operations'][op_id]
        assert not previous.get('cancelled'),'Memory capacity request was cancelled on the host'
        assert previous=={'target_bytes':target,'expected_revision':expected},'Memory request key reused'
        return status(config)
    assert expected==r['revision'],'Memory capacity changed; refresh before adopting'
    assert type(target) is int and target>=0 and target%GIB==0
    proof=status(config);assert target<=proof['safe_bytes'],'Memory target exceeds physical RAM after system reserves'
    assert proof['arc_ready'],'Repair ARC enforcement before adopting capacity'
    assert target>=config['_reserve_record']['state']['additional']['memory_bytes'],'Lower additional memory reserves before adopting this smaller envelope'
    # Release only genuinely reclaimed cache. Neither a new cap nor extra RAM
    # alone authorizes promising memory still occupied by old cache buffers.
    arc_policy.reclaim_for_release(config,config['_reserve_record'],lambda v:host.resource_reserves.write_record(host,v))
    r['revision']+=1;r['bytes']=target;r['operations'][op_id]={'target_bytes':target,'expected_revision':expected}
    host.save(host.STATE/'memory-capacity.json',r)
    fd=__import__('os').open(host.STATE,__import__('os').O_RDONLY|__import__('os').O_DIRECTORY)
    try:__import__('os').fsync(fd)
    finally:__import__('os').close(fd)
    base['capacity']['memory_bytes']=target;base['_memory_capacity']=r
    return status(config)


def running_guard(config,vms):
    proof=status(config)
    assert proof['arc_ready'],'conflict: '+str(proof['error'])
    extra=config.get('_reserve_record',{}).get('state',{}).get('additional',{}).get('memory_bytes',0)
    capacity=max(0,min(proof['reviewed_bytes'],proof['safe_bytes'])-extra)
    assert sum(v['resources']['memory_bytes'] for v in vms if v['observed']!='deleted')<=capacity,'conflict: held VM memory exceeds current physical capacity; stop or resize VMs before starting workloads'


def cancel(host,config,operation):
    r=record(host,config['_reserve_base']);op_id=str(uuid.UUID(operation['id']))
    previous=r['operations'].get(op_id)
    assert previous is None or previous.get('cancelled'),'Capacity effects have begun; finish recovery instead of cancelling'
    assert r['revision']==operation['expected_revision'],'Capacity revision changed; finish recovery first'
    r['operations'][op_id]={'cancelled':True,'target_bytes':operation['target_bytes'],'expected_revision':operation['expected_revision']}
    host.save(host.STATE/'memory-capacity.json',r)
    fd=__import__('os').open(host.STATE,__import__('os').O_RDONLY|__import__('os').O_DIRECTORY)
    try:__import__('os').fsync(fd)
    finally:__import__('os').close(fd)
    return {'operation_id':op_id,'status':status(config)}
