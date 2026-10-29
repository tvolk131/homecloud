"""Small Linux OpenZFS ARC boundary model, not a filesystem or cache emulator.

Based on OpenZFS 2.4.1 arc_tuning_update/arc_init. Real probes must calibrate
accepted writes/effective stats. Time and reclaim are explicit test events.
"""
GIB=1024**3
MIB=1024**2

class ArcModel:
    def __init__(self,physical=16*GIB,cap=2*GIB):
        self.physical=physical;self.maximum=cap;self.minimum=physical//32
        self.parameter=cap;self.minimum_parameter=0;self.size=cap
        self.events=[];self.ignore_minimum=False;self.fail_persistence=False
    def observe(self):
        return {'size_bytes':self.size,'minimum_bytes':self.minimum,'maximum_bytes':self.maximum,'parameter_bytes':self.parameter,'automatic_maximum_bytes':max(self.physical-GIB,self.physical*5//8),'version':'2.4.1-model'}
    def run(self,*args,**kwargs):
        self.events.append(('command',args))
        if self.fail_persistence:raise RuntimeError('injected initramfs failure')
    def resize_boot(self,physical,persistent_cap,persistent_minimum=0):
        self.physical=physical;self.parameter=persistent_cap;self.minimum_parameter=persistent_minimum
        self.maximum=persistent_cap;self.minimum=persistent_minimum or physical//32
        if self.minimum>=self.maximum:self.minimum=max(32*MIB,self.maximum//2)
        self.size=0;self.events.append(('boot',physical))
    def reclaim(self,bytes):self.size=bytes;self.events.append(('reclaim',bytes))
    def __truediv__(self,name):return Knob(self,name)

class Knob:
    def __init__(self,model,name):self.model=model;self.name=name
    def read_text(self):return str(self.model.minimum_parameter if self.name=='zfs_arc_min' else self.model.parameter)
    def write_text(self,text):
        value=int(text);m=self.model;m.events.append((self.name,value))
        if self.name=='zfs_arc_min':
            m.minimum_parameter=value
            if not m.ignore_minimum and 32*MIB<=value<=m.maximum:m.minimum=value
        elif self.name=='zfs_arc_max':
            m.parameter=value
            if 64*MIB<=value<m.physical and value>m.minimum:m.maximum=value
        else:raise AssertionError('Unmodeled knob '+self.name)
