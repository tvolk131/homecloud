use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmSetupStage {
    Creating,
    Starting,
    GuestAgent,
    GuestSetup,
    GpuDriver,
    SshIdentity,
}
impl VmSetupStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Creating => "Preparing the VM disk and configuration",
            Self::Starting => "Starting the VM",
            Self::GuestAgent => "Waiting for the guest agent",
            Self::GuestSetup => "Installing guest tools",
            Self::GpuDriver => "Preparing GPU drivers",
            Self::SshIdentity => "Verifying SSH identity",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmSetupProgress {
    pub stage: VmSetupStage,
    pub since: u64,
}
/// On-demand, bounded guest setup logs. Never stored in the controller database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmSetupLog {
    pub name: String,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmDiagnostics {
    pub vm_id: Uuid,
    pub progress: Option<VmSetupProgress>,
    pub power_state: String,
    pub checks: Vec<String>,
    pub logs: Vec<VmSetupLog>,
}
