use super::*;

/// A condition derived from durable claims and the latest verified host policy.
/// It is not dismissible: recovery clears the condition, not an acknowledgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCondition {
    Healthy,
    PromisesExceedCapacity,
    AllocationsExceedCapacity,
    ArcNotEnforced,
    Unavailable,
}
impl MemoryCondition {
    pub fn title(self) -> &'static str {
        match self {
            Self::Healthy => "Memory policy healthy",
            Self::PromisesExceedCapacity => "Memory promises exceed capacity",
            Self::AllocationsExceedCapacity => "RAM shortage — VM startup blocked",
            Self::ArcNotEnforced => "ZFS cache policy needs repair",
            Self::Unavailable => "Host memory safety could not be verified",
        }
    }
    pub fn explanation(self) -> &'static str {
        match self {
            Self::Healthy => "Memory allocations fit the verified host budget.",
            Self::PromisesExceedCapacity => {
                "Existing allocations can resume. New memory claims wait until grants fit the host budget."
            }
            Self::AllocationsExceedCapacity => {
                "Held VM memory exceeds the host budget. Automatic startup waits for an administrator to resolve the shortage."
            }
            Self::ArcNotEnforced => {
                "VM startup waits until the effective ZFS cache limit matches the backed policy."
            }
            Self::Unavailable => {
                "Allocation is paused until the host policy can be verified. Existing claims are retained."
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryHealthDetails {
    pub capacity_bytes: u64,
    pub promised_bytes: u64,
    pub held_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostMemoryHealth {
    pub condition: MemoryCondition,
    /// Aggregate promises/claims are visible only to administrators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<MemoryHealthDetails>,
}
impl HostMemoryHealth {
    pub fn blocks_start(&self, vm: &Vm) -> bool {
        match self.condition {
            MemoryCondition::Healthy => false,
            MemoryCondition::PromisesExceedCapacity => !vm.has_compute(),
            _ => true,
        }
    }
    pub fn waiting(&self, vm: &Vm) -> bool {
        vm.desired == Lifecycle::Running && vm.observed == "stopped" && self.blocks_start(vm)
    }
}
