use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneMode {
    ManagedHost,
    Virtualization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualizationSource {
    Simulated,
    Proxmox,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VirtualizationInventory {
    pub source: VirtualizationSource,
    pub nodes: Vec<VirtualizationNode>,
    pub guests: Vec<VirtualizationGuest>,
    pub storage: Vec<VirtualizationStorage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VirtualizationNode {
    pub name: String,
    pub status: String,
    pub cpu_threads: Option<u32>,
    pub cpu_usage_ratio: Option<f64>,
    pub memory_capacity_bytes: Option<u64>,
    pub memory_used_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualizationGuestKind {
    VirtualMachine,
    Container,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualizationGuest {
    pub node: String,
    pub vmid: u32,
    pub kind: VirtualizationGuestKind,
    pub name: Option<String>,
    pub status: String,
    pub template: bool,
    pub cpu_limit: Option<u32>,
    pub memory_limit_bytes: Option<u64>,
    pub memory_used_bytes: Option<u64>,
    pub reported_disk_capacity_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualizationStorage {
    pub node: String,
    pub name: String,
    pub status: String,
    pub capacity_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualizationGuestIdentity {
    pub node: String,
    pub vmid: u32,
    pub kind: VirtualizationGuestKind,
}

impl VirtualizationGuestIdentity {
    pub fn validate(&self) -> Result<(), VirtualizationIdentityError> {
        if self.node.is_empty()
            || self.node.len() > 64
            || !self
                .node
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            || !self.node.as_bytes()[0].is_ascii_alphanumeric()
        {
            return Err(VirtualizationIdentityError::Node);
        }
        if !(100..=999_999_999).contains(&self.vmid) {
            return Err(VirtualizationIdentityError::VmId);
        }
        if self.kind != VirtualizationGuestKind::VirtualMachine {
            return Err(VirtualizationIdentityError::Kind);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VirtualizationIdentityError {
    #[error("Invalid Proxmox node name")]
    Node,
    #[error("Invalid Proxmox VM ID")]
    VmId,
    #[error("Power controls currently support QEMU VMs only")]
    Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualizationPowerAction {
    Start,
    Shutdown,
    ForceStop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualizationOperationPhase {
    Submitting,
    Running,
    Succeeded,
    Failed,
    Unknown,
    Acknowledged,
}

impl VirtualizationOperationPhase {
    pub fn blocks_guest(self) -> bool {
        matches!(self, Self::Submitting | Self::Running | Self::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualizationOperation {
    pub id: Uuid,
    pub requested_by: String,
    pub guest: VirtualizationGuestIdentity,
    pub action: VirtualizationPowerAction,
    pub phase: VirtualizationOperationPhase,
    pub task_id: Option<String>,
    pub error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}
