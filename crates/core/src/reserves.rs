use super::*;

/// Additional reserves above the explicit operating minimum. The public editor
/// presents minimum + additional as a total, including its ARC allowance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReserves {
    pub revision: u64,
    pub additional: Resources,
    pub core_groups: Vec<Vec<u32>>,
    #[serde(default)]
    pub arc: Option<ArcMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ArcMode {
    Automatic,
    Custom { max_bytes: u64 },
}
impl ArcMode {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Custom { max_bytes } = self
            && *max_bytes < 64 * 1024 * 1024
        {
            return Err("ARC maximum must be at least 64 MiB; choose Automatic explicitly".into());
        }
        Ok(())
    }
}

/// Root-reviewed operating headroom. The remaining system reserve bounds ARC;
/// automatic sizing uses this recommendation, never guest RSS or native defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostMemoryPolicy {
    pub operating_floor_bytes: u64,
    pub recommended_arc_bytes: u64,
    pub minimum_system_bytes: u64,
}
impl HostMemoryPolicy {
    pub fn allowance(self, total: u64) -> Result<u64, String> {
        total
            .checked_sub(self.operating_floor_bytes)
            .ok_or_else(|| "System reserve is below operating headroom".into())
    }
    pub fn maximum(self, total: u64, mode: &ArcMode) -> Result<u64, String> {
        mode.validate()?;
        let allowance = self.allowance(total)?;
        let maximum = match mode {
            ArcMode::Automatic => self.recommended_arc_bytes.min(allowance),
            ArcMode::Custom { max_bytes } => *max_bytes,
        };
        if maximum < 64 * 1024 * 1024 || maximum > allowance {
            return Err(
                "ARC exceeds its allowance; lower the cap or increase total system reserves".into(),
            );
        }
        Ok(maximum)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArcTelemetry {
    pub size_bytes: u64,
    pub minimum_bytes: u64,
    pub maximum_bytes: u64,
    pub parameter_bytes: u64,
    pub automatic_maximum_bytes: Option<u64>,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservePhase {
    Pending,
    Applying,
    NeedsReview,
    Succeeded,
    Cancelled,
}
impl ReservePhase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReserveOperation {
    pub id: Uuid,
    pub actor: Uuid,
    pub request_key: Uuid,
    pub before: HostReserves,
    pub target: HostReserves,
    pub phase: ReservePhase,
    pub error: Option<String>,
}

/// Attested by the fixed privileged adapter, not supplied by a network client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReservePolicy {
    /// Reviewed subnets occupied by this host uplink, unavailable for VM bridges.
    #[serde(default)]
    pub excluded_network_slots: Vec<u32>,
    #[serde(default)]
    pub memory: Option<MemoryCapacityStatus>,
    pub envelope: Resources,
    pub core_groups: Vec<Vec<u32>>,
    pub disk_overhead_gib_per_vm: u64,
    pub base_pool_free_bytes: u64,
    pub state: HostReserves,
    pub applying: bool,
    pub operation_id: Option<Uuid>,
    #[serde(default)]
    pub minimum_reserves: Resources,
    #[serde(default)]
    pub arc: Option<ArcTelemetry>,
    #[serde(default)]
    pub memory_policy: Option<HostMemoryPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostReserveView {
    #[serde(default)]
    pub memory_operation: Option<MemoryCapacityOperation>,
    #[serde(default)]
    pub memory: Option<MemoryCapacityStatus>,
    pub active: HostReserves,
    pub operation: Option<HostReserveOperation>,
    pub envelope: Resources,
    pub protected_backing: Resources,
    pub base_pool_free_bytes: u64,
    pub can_discard: bool,
    #[serde(default)]
    pub minimum_reserves: Resources,
    #[serde(default)]
    pub arc: Option<ArcTelemetry>,
    #[serde(default)]
    pub memory_policy: Option<HostMemoryPolicy>,
}

impl Resources {
    pub fn checked_sub(self, other: Self) -> Result<Self, String> {
        Ok(Self {
            memory_bytes: self
                .memory_bytes
                .checked_sub(other.memory_bytes)
                .ok_or("Memory reserve exceeds the reviewed capacity")?,
            cpu_cores: self
                .cpu_cores
                .checked_sub(other.cpu_cores)
                .ok_or("CPU reserve exceeds the reviewed capacity")?,
            disk_bytes: self
                .disk_bytes
                .checked_sub(other.disk_bytes)
                .ok_or("Disk reserve exceeds the reviewed capacity")?,
        })
    }
}

/// Fresh physical attestation, separate from durable grants and held claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCapacityStatus {
    pub revision: u64,
    pub reviewed_bytes: u64,
    pub safe_bytes: u64,
    pub physical_bytes: u64,
    pub minimum_system_bytes: u64,
    pub boot_id: String,
    pub observed_at: u64,
    pub arc_ready: bool,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCapacityOperation {
    pub id: Uuid,
    pub actor: Uuid,
    pub expected_revision: u64,
    pub target_bytes: u64,
    pub succeeded: bool,
    #[serde(default)]
    pub cancelled: bool,
}
