use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostShutdownPhase {
    Draining,
    PoweroffPending,
    PoweroffRequested,
    NeedsReview,
    Cancelled,
    Finished,
}
impl HostShutdownPhase {
    pub fn blocks_work(self) -> bool {
        !matches!(self, Self::Cancelled | Self::Finished)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostShutdownOperation {
    pub id: Uuid,
    pub actor: Uuid,
    pub boot_id: String,
    pub revision: u64,
    pub phase: HostShutdownPhase,
    pub vm_ids: Vec<Uuid>,
    pub remaining: Vec<Uuid>,
    /// Once attempted, an absent acknowledgement cannot make cancellation safe.
    pub poweroff_attempted: bool,
    #[serde(default)]
    pub poweroff_requested_at: Option<u64>,
    pub error: Option<String>,
}
