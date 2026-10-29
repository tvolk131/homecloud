use crate::{Lifecycle, Vm, VmSpec};
use serde::{Deserialize, Serialize};

/// Keeping a GPU preserves both the active assignment and next-start preference.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GpuEdit {
    #[default]
    Keep,
    /// Legacy single-device edits remain readable in operation history.
    Set {
        pci: Option<String>,
    },
    SetMany {
        pcis: Vec<String>,
    },
}

impl GpuEdit {
    pub fn target_gpus(&self, vm: &Vm) -> Vec<String> {
        match self {
            GpuEdit::Keep if vm.desired == Lifecycle::Running => vm.gpus.clone(),
            GpuEdit::Keep => vm.preferred_gpus.clone(),
            GpuEdit::Set { pci } => pci.iter().cloned().collect(),
            GpuEdit::SetMany { pcis } => crate::canonical_gpu_set(pcis.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceEdit {
    pub name: String,
    pub memory_gib: u64,
    pub cores: u64,
    pub disk_gib: u64,
    #[serde(default)]
    pub gpu: GpuEdit,
}
impl WorkspaceEdit {
    pub fn spec(&self, vm: &Vm) -> VmSpec {
        VmSpec {
            name: self.name.trim().into(),
            memory_gib: self.memory_gib,
            cores: self.cores,
            disk_gib: self.disk_gib,
            ..vm.spec.clone()
        }
    }
    pub fn target_gpus(&self, vm: &Vm) -> Vec<String> {
        self.gpu.target_gpus(vm)
    }
    pub fn sizes_changed(&self, vm: &Vm) -> bool {
        (self.memory_gib, self.cores, self.disk_gib)
            != (vm.spec.memory_gib, vm.spec.cores, vm.spec.disk_gib)
    }
    pub fn needs_host(&self, vm: &Vm) -> bool {
        self.sizes_changed(vm)
            || (vm.desired == Lifecycle::Running && self.target_gpus(vm) != vm.gpus)
    }
    pub fn requires_restart(&self, vm: &Vm) -> bool {
        vm.desired == Lifecycle::Running && self.needs_host(vm)
    }
    pub fn changed(&self, vm: &Vm) -> bool {
        self.name.trim() != vm.spec.name
            || self.needs_host(vm)
            || (!matches!(self.gpu, GpuEdit::Keep) && self.target_gpus(vm) != vm.preferred_gpus)
    }
}
