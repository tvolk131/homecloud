"""Bound ARC inside protected system reserves; all writable paths are fixed."""
import os
from pathlib import Path
import re
import time

GIB=1024**3
PARAMS=Path('/sys/module/zfs/parameters')
STATS=Path('/proc/spl/kstat/zfs/arcstats')
VERSION=Path('/sys/module/zfs/version')
CONFIG=Path('/etc/modprobe.d/zz-homecloud-arc.conf')
MEMINFO=Path('/proc/meminfo')


def physical_memory():return os.sysconf('SC_PHYS_PAGES')*os.sysconf('SC_PAGE_SIZE')


def automatic_maximum(version,total):
    # Read-only native-default telemetry; Homecloud Automatic does not use it.
    match=re.match(r'^2\.(\d+)\.',version)
    if not match:return None
    minor=int(match[1])
    if minor<=2:return total//2
    if minor in (3,4):return max(total-GIB,total*5//8)
    return None


def observe():
    if not STATS.exists():return None
    stats={l.split()[0]:int(l.split()[-1]) for l in STATS.read_text().splitlines()[2:]}
    version=VERSION.read_text().strip()
    return {'size_bytes':stats['size'],'minimum_bytes':stats['c_min'],'maximum_bytes':stats['c_max'],
            'parameter_bytes':int((PARAMS/'zfs_arc_max').read_text()),
            'automatic_maximum_bytes':automatic_maximum(version,physical_memory()),'version':version}


def policy(config):
    base=config.get('_reserve_base',config)
    floor=base.get('memory_operating_floor_bytes');recommended=base.get('arc_recommended_maximum_bytes')
    if floor is None or recommended is None:return None
    minimum=base['minimum_reserves']['memory_bytes']
    assert type(floor) is int and type(recommended) is int and floor>0 and recommended>=64*1024**2
    assert floor+recommended<=minimum,'Operating floor and recommended cache exceed the minimum system reserve'
    return {'operating_floor_bytes':floor,'recommended_arc_bytes':recommended,'minimum_system_bytes':minimum}


def allowance(config):
    p=policy(config);assert p is not None,'Host ARC budget is unavailable; review the host profile'
    extra=config.get('_reserve_record',{}).get('state',{}).get('additional',{}).get('memory_bytes',0)
    return p['minimum_system_bytes']+extra-p['operating_floor_bytes']


def maximum(mode,observed,config):
    assert mode['mode'] in ('automatic','custom'),'Unknown ARC mode'
    budget=allowance(config)
    cap=min(policy(config)['recommended_arc_bytes'],budget) if mode['mode']=='automatic' else mode['max_bytes']
    assert type(cap) is int and 64*1024**2<=cap<physical_memory(),'ARC limit must be positive and below physical memory'
    assert cap<=budget,'ARC exceeds its allowance; lower the cap or increase total system reserves'
    return cap


def minimum_target(observed,cap):
    # Keep ZFS's effective floor when it fits. A previously required managed
    # floor stays durable across reboot; do not silently restore a larger floor.
    previous=re.search(r'\bzfs_arc_min=(\d+)',CONFIG.read_text()) if CONFIG.exists() else None
    if previous:return min(int(previous[1]),cap//2)
    return max(32*1024**2,cap//2) if observed['minimum_bytes']>=cap else None


def desired_text(mode,config,observed=None):
    observed=observed if observed is not None else observe();cap=maximum(mode,observed,config)
    minimum=minimum_target(observed,cap)
    return ('# Managed by Homecloud. Cache is inside the system reserve.\noptions zfs zfs_arc_max='+str(cap)
            +(' zfs_arc_min='+str(minimum) if minimum is not None else '')+'\n')


def validate(config,mode):
    if mode is None:return
    observed=observe();assert observed is not None,'ARC telemetry is unavailable'
    maximum(mode,observed,config)
    for path in CONFIG.parent.glob('*.conf'):
        if path==CONFIG:continue
        for line in path.read_text().splitlines():
            content=line.split('#',1)[0].strip()
            assert not (re.match(r'^options\s+zfs\s',content) and any(key in content for key in ('zfs_arc_max=','zfs_arc_min='))),'Conflicting ARC setting in '+str(path)+'; adopt it through local host setup'


def _enforced(mode,config):
    if mode is None:
        if policy(config) is None:return True
        observed=observe()
        return observed is not None and observed['maximum_bytes']<=allowance(config)
    observed=observe()
    return observed is not None and observed['maximum_bytes']==maximum(mode,observed,config) and observed['minimum_bytes']<observed['maximum_bytes'] and CONFIG.exists() and CONFIG.read_text()==desired_text(mode,config,observed)


def apply(host,config,mode,persist=True):
    if mode is None:return
    validate(config,mode);observed=observe();cap=maximum(mode,observed,config)
    if persist:
        CONFIG.parent.mkdir(parents=True,exist_ok=True);temp=CONFIG.with_suffix('.tmp')
        with temp.open('w') as stream:
            stream.write(desired_text(mode,config,observed));stream.flush();os.fsync(stream.fileno())
        temp.chmod(0o644);temp.replace(CONFIG)
        fd=os.open(CONFIG.parent,os.O_RDONLY|os.O_DIRECTORY)
        try:os.fsync(fd)
        finally:os.close(fd)
        host.run('update-initramfs','-u','-k','all',timeout=180)
    else:assert CONFIG.exists() and CONFIG.read_text()==desired_text(mode,config,observed),'Persistent ARC policy differs; repair locally'
    minimum=minimum_target(observed,cap)
    # Linux OpenZFS validates a new max against the current min. Lower the min
    # first, then the max, and verify effective stats rather than sysfs alone.
    if minimum is not None and observed['minimum_bytes']!=minimum:
        (PARAMS/'zfs_arc_min').write_text(str(minimum))
        assert observe()['minimum_bytes']==minimum,'ZFS did not apply the coordinated ARC minimum; capacity remains fenced'
    if observed['maximum_bytes']!=cap or observed['parameter_bytes']!=cap:(PARAMS/'zfs_arc_max').write_text(str(cap))
    assert enforced(mode,config),'ZFS did not apply the requested ARC maximum'


def restore_reclaim(record,save):
    if 'arc_previous_sys_free' not in record:return
    (PARAMS/'zfs_arc_sys_free').write_text(str(record['arc_previous_sys_free']))
    del record['arc_previous_sys_free'];save(record)


def reclaim_for_release(config,record,save,timeout=30):
    """Ask ARC to yield before releasing capacity; never allocate fake pressure RAM."""
    if policy(config) is None:return
    limit=allowance(config);observed=observe()
    restore_reclaim(record,save)
    if observed['size_bytes']<=limit:return
    knob=PARAMS/'zfs_arc_sys_free'
    free=int(next(l.split()[1] for l in MEMINFO.read_text().splitlines() if l.startswith('MemFree:')))*1024
    original=int(knob.read_text());record['arc_previous_sys_free']=original;save(record)
    # This temporary free-memory target encourages ARC's own reclaim thread.
    # Its original value is journaled before the write and restored on recovery.
    target=min(physical_memory()-1,max(original,free+observed['size_bytes']-limit+64*1024**2))
    try:
        knob.write_text(str(target));deadline=time.monotonic()+timeout
        while time.monotonic()<deadline:
            if observe()['size_bytes']<=limit:return
            time.sleep(.1)
        raise RuntimeError('pending: waiting for ARC reclamation before releasing system memory; capacity remains fenced')
    finally:restore_reclaim(record,save)


def enforced(mode,config):
    try:return _enforced(mode,config)
    except (AssertionError,OSError,ValueError):return False
