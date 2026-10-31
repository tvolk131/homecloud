mod setup;
mod shutdown;
pub use setup::*;
pub use shutdown::*;
mod apps;
mod edit;
mod memory_health;
mod reserves;
mod storage;
pub use memory_health::*;
pub use storage::*;
mod vm;
pub use apps::*;
pub use edit::*;
pub use reserves::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub use vm::*;

pub const CONTROL_ALPN: &[u8] = b"homecloud/control/2";
pub const SSH_ALPN: &[u8] = b"homecloud/ssh/2";
pub const MAX_FRAME: usize = 1024 * 1024;
pub const GIB: u64 = 1024 * 1024 * 1024;
pub const MANAGED_HOST_DISK_GIB: u64 = 32;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    pub memory_bytes: u64,
    pub cpu_cores: u64,
    /// Physical backing, including the configured per-disk overhead budget.
    pub disk_bytes: u64,
}
impl Resources {
    pub fn checked_add(self, other: Self) -> Result<Self, String> {
        Ok(Self {
            memory_bytes: self
                .memory_bytes
                .checked_add(other.memory_bytes)
                .ok_or("Memory overflow")?,
            cpu_cores: self
                .cpu_cores
                .checked_add(other.cpu_cores)
                .ok_or("CPU overflow")?,
            disk_bytes: self
                .disk_bytes
                .checked_add(other.disk_bytes)
                .ok_or("Disk overflow")?,
        })
    }
    pub fn max(self, other: Self) -> Self {
        Self {
            memory_bytes: self.memory_bytes.max(other.memory_bytes),
            cpu_cores: self.cpu_cores.max(other.cpu_cores),
            disk_bytes: self.disk_bytes.max(other.disk_bytes),
        }
    }
    pub fn disk_only(self) -> Self {
        Self {
            memory_bytes: 0,
            cpu_cores: 0,
            disk_bytes: self.disk_bytes,
        }
    }
    /// An over-quota allocation may stay unchanged or shrink in each dimension.
    pub fn admit(self, after: Self, quota: Self) -> Result<(), String> {
        for (name, before, next, ceiling) in [
            (
                "Memory",
                self.memory_bytes,
                after.memory_bytes,
                quota.memory_bytes,
            ),
            ("CPU", self.cpu_cores, after.cpu_cores, quota.cpu_cores),
            (
                "Disk backing",
                self.disk_bytes,
                after.disk_bytes,
                quota.disk_bytes,
            ),
        ] {
            if next > before && next > ceiling {
                return Err(format!("{name} quota exceeded"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub name: String,
    pub role: Role,
    pub active: bool,
    pub grant: Resources,
    pub committed: Resources,
    pub gpu_allowed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub peer: String,
    pub user_id: Uuid,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Running,
    Stopped,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmSpec {
    #[serde(default)]
    pub applications: Vec<ApplicationKind>,
    pub name: String,
    pub memory_gib: u64,
    pub cores: u64,
    pub disk_gib: u64,
    pub ssh_public_key: String,
}
impl VmSpec {
    pub fn managed_host(&self) -> bool {
        self.applications.contains(&ApplicationKind::HomecloudHost)
    }
    pub fn application_disk_gib(&self) -> u64 {
        if self.managed_host() {
            MANAGED_HOST_DISK_GIB
        } else {
            0
        }
    }
    pub fn resources(&self, overhead_gib: u64) -> Result<Resources, String> {
        if !(1..=1024).contains(&self.memory_gib)
            || !(1..=256).contains(&self.cores)
            || !(10..=65536).contains(&self.disk_gib)
        {
            return Err("Invalid VM size: minimum 1 GiB RAM, one core, and 10 GiB disk".into());
        }
        if self.name.trim().is_empty()
            || self.name.len() > 80
            || self.name.chars().any(char::is_control)
        {
            return Err("VM name must contain 1–80 characters".into());
        }
        if self.applications.len() > 1 {
            return Err("Only one instance of each supported application is allowed".into());
        }
        if self.managed_host() && (self.memory_gib < 8 || self.cores < 2 || self.disk_gib < 32) {
            return Err(
                "Homecloud hosts need at least 8 GiB RAM, two cores, and a 32 GiB root disk".into(),
            );
        }
        validate_ssh_key(&self.ssh_public_key)?;
        Ok(Resources {
            memory_bytes: self.memory_gib * GIB,
            cpu_cores: self.cores,
            disk_bytes: self
                .disk_gib
                .checked_add(self.application_disk_gib())
                .and_then(|x| x.checked_add(overhead_gib))
                .and_then(|x| x.checked_mul(GIB))
                .ok_or("Disk overflow")?,
        })
    }
}
pub fn validate_ssh_key(key: &str) -> Result<(), String> {
    let mut parts = key.split_whitespace();
    let kind = parts.next().unwrap_or_default();
    let body = parts.next().unwrap_or_default();
    if !["ssh-ed25519", "ssh-rsa", "ecdsa-sha2-nistp256"].contains(&kind)
        || body.len() < 32
        || key.len() > 8192
        || key.contains(['\n', '\r'])
        || !body
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
    {
        return Err("A single OpenSSH public key is required".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    #[serde(default)]
    pub progress: Option<VmSetupProgress>,
    pub id: Uuid,
    pub vm_id: Uuid,
    pub actor: Uuid,
    pub request: Mutation,
    pub phase: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mutation {
    Create {
        spec: VmSpec,
    },
    State {
        vm_id: Uuid,
        desired: Lifecycle,
    },
    Start {
        vm_id: Uuid,
        /// Explicit consent for this boot; preserve the remembered preference.
        without_gpu: bool,
    },
    /// Explicit, user-confirmed power-off. Never inferred from a timeout.
    ForceStop {
        vm_id: Uuid,
    },
    Edit {
        vm_id: Uuid,
        changes: WorkspaceEdit,
        restart_confirmed: bool,
    },
    Resize {
        vm_id: Uuid,
        memory_gib: u64,
        cores: u64,
        disk_gib: u64,
    },
    Gpu {
        vm_id: Uuid,
        pci: Option<String>,
    },
}
impl Mutation {
    pub fn stops_vm(&self) -> bool {
        matches!(
            self,
            Self::ForceStop { .. }
                | Self::State {
                    desired: Lifecycle::Stopped,
                    ..
                }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("GPU {pci} is unavailable: {reason}")]
pub struct GpuUnavailable {
    pub vm_id: Uuid,
    pub pci: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Readiness {
    pub state: String,
    pub evidence: Vec<String>,
    pub next_steps: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gpu {
    pub pci: String,
    pub model: String,
    pub driver: Option<String>,
    pub iommu_group: Option<u32>,
    pub host_display: bool,
    pub readiness: Readiness,
    #[serde(default)]
    pub assigned_vm: Option<Uuid>,
    #[serde(default)]
    pub telemetry: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostInventory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageInventory>,
    /// Present only on adapters that support safe host shutdown.
    #[serde(default)]
    pub shutdown_boot_id: Option<String>,
    pub hostname: String,
    pub cpu_model: String,
    pub physical_cores: usize,
    pub memory_bytes: u64,
    pub pool: String,
    pub pool_available_bytes: u64,
    /// Root-owned host policy; omitted by older adapters. This is an operating
    /// free-space floor, not a dataset reservation or total pool capacity.
    #[serde(default)]
    pub minimum_pool_available_bytes: Option<u64>,
    pub pool_health: String,
    pub iommu: Option<Readiness>,
    pub gpus: Vec<Gpu>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub host_shutdown: Option<HostShutdownOperation>,
    /// Scoped condition for every user; aggregate details are admin-only.
    #[serde(default)]
    pub memory_health: Option<HostMemoryHealth>,
    /// Admin-only reserve configuration and recovery state.
    #[serde(default)]
    pub host_reserves: Option<HostReserveView>,
    /// Admission overhead per VM, separate from guest-visible disk capacity.
    #[serde(default = "legacy_disk_overhead_gib")]
    pub disk_overhead_gib_per_vm: u64,
    #[serde(default)]
    pub applications: Vec<ManagedApplication>,
    #[serde(default)]
    pub application_operations: Vec<ApplicationOperation>,
    pub me: User,
    pub users: Vec<User>,
    pub devices: Vec<Device>,
    pub vms: Vec<Vm>,
    pub operations: Vec<Operation>,
    pub inventory: HostInventory,
    pub capacity: Resources,
}
fn legacy_disk_overhead_gib() -> u64 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    ShutdownHost {
        idempotency_key: Uuid,
        expected_boot_id: String,
    },
    RetryHostShutdown {
        operation_id: Uuid,
    },
    CancelHostShutdown {
        operation_id: Uuid,
    },
    Enroll {
        token: String,
    },
    Snapshot,
    CancelMemoryCapacity {
        operation_id: Uuid,
    },
    AdoptMemoryCapacity {
        expected_revision: u64,
        target_bytes: u64,
        idempotency_key: Uuid,
    },
    SetHostReserves {
        reserves: Resources,
        #[serde(default)]
        arc: Option<ArcMode>,
        expected_revision: u64,
        idempotency_key: Uuid,
    },
    RetryHostReserves {
        operation_id: Uuid,
    },
    DiscardHostReserves {
        operation_id: Uuid,
    },
    EnsureApplication {
        vm_id: Uuid,
        application_kind: ApplicationKind,
        idempotency_key: Uuid,
    },
    StartApplicationSetup {
        application_id: Uuid,
        #[serde(default)]
        setup_action: ApplicationSetupAction,
        idempotency_key: Uuid,
    },
    SubmitApplicationCode {
        application_id: Uuid,
        operation_id: Uuid,
        code: AuthorizationCode,
    },
    Invite {
        name: String,
        role: Role,
        grant: Resources,
        gpu_allowed: bool,
        expires_hours: u64,
    },
    InviteDevice {
        user_id: Uuid,
        expires_hours: u64,
    },
    Grant {
        user_id: Uuid,
        grant: Resources,
        gpu_allowed: bool,
    },
    RevokeDevice {
        peer: String,
    },
    SetAccountActive {
        user_id: Uuid,
        active: bool,
    },
    /// Recoverable creation with durable acceptance/rejection receipts.
    CreateVm {
        idempotency_key: Uuid,
        spec: VmSpec,
    },
    Mutate {
        idempotency_key: Uuid,
        mutation: Mutation,
    },
    VmDiagnostics {
        vm_id: Uuid,
    },
    RetryOperation {
        operation_id: Uuid,
    },
    Ssh {
        vm_id: Uuid,
    },
}
impl Request {
    /// Resource promises and guest/host effects pause together during shutdown.
    /// Read-only and account/device security actions remain available.
    pub fn changes_host_workload(&self) -> bool {
        match self {
            Self::CancelMemoryCapacity { .. }
            | Self::AdoptMemoryCapacity { .. }
            | Self::SetHostReserves { .. }
            | Self::RetryHostReserves { .. }
            | Self::DiscardHostReserves { .. }
            | Self::EnsureApplication { .. }
            | Self::StartApplicationSetup { .. }
            | Self::SubmitApplicationCode { .. }
            | Self::Invite { .. }
            | Self::Grant { .. }
            | Self::CreateVm { .. }
            | Self::Mutate { .. }
            | Self::RetryOperation { .. } => true,
            Self::ShutdownHost { .. }
            | Self::RetryHostShutdown { .. }
            | Self::CancelHostShutdown { .. }
            | Self::Enroll { .. }
            | Self::Snapshot
            | Self::InviteDevice { .. }
            | Self::RevokeDevice { .. }
            | Self::SetAccountActive { .. }
            | Self::VmDiagnostics { .. }
            | Self::Ssh { .. } => false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Response {
    HostShutdownOperation(HostShutdownOperation),
    VmDiagnostics(VmDiagnostics),
    Enrolled(User),
    HostReserveOperation(HostReserveOperation),
    Snapshot(Box<Snapshot>),
    Invitation {
        token: String,
        user_id: Uuid,
        expires_at: u64,
    },
    Operation(Operation),
    CreateRejected {
        idempotency_key: Uuid,
        message: String,
    },
    ApplicationOperation(ApplicationOperation),
    GpuUnavailable(GpuUnavailable),
    SshReady,
    Ok,
    Error {
        message: String,
    },
}
impl Response {
    pub fn error(error: impl std::fmt::Display) -> Self {
        Self::Error {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_inventory_does_not_invent_a_pool_free_space_floor() {
        let mut inventory = serde_json::to_value(HostInventory::default()).unwrap();
        inventory
            .as_object_mut()
            .unwrap()
            .remove("minimum_pool_available_bytes");
        let old: HostInventory = serde_json::from_value(inventory.clone()).unwrap();
        assert_eq!(old.minimum_pool_available_bytes, None);
        inventory["minimum_pool_available_bytes"] = serde_json::json!(30 * GIB);
        let updated: HostInventory = serde_json::from_value(inventory).unwrap();
        assert_eq!(updated.minimum_pool_available_bytes, Some(30 * GIB));
    }

    #[test]
    fn older_inventory_omits_storage_instead_of_inventing_an_empty_success() {
        let mut inventory = serde_json::to_value(HostInventory::default()).unwrap();
        inventory.as_object_mut().unwrap().remove("storage");
        let old: HostInventory = serde_json::from_value(inventory).unwrap();
        assert!(old.storage.is_none());
    }

    #[test]
    fn provider_return_codes_are_redacted_in_request_and_host_command_diagnostics() {
        let code = AuthorizationCode("PRIVATE-AUTHORIZATION-CODE".into());
        let request = Request::SubmitApplicationCode {
            application_id: Uuid::nil(),
            operation_id: Uuid::nil(),
            code: code.clone(),
        };
        assert!(!format!("{request:?}").contains(&code.0));
        assert!(serde_json::to_string(&request).unwrap().contains(&code.0));
    }
    #[test]
    fn quota_reductions_are_per_dimension_and_prospective() {
        let before = Resources {
            memory_bytes: 24,
            cpu_cores: 2,
            disk_bytes: 32,
        };
        let quota = Resources {
            memory_bytes: 16,
            cpu_cores: 4,
            disk_bytes: 16,
        };
        assert!(
            before
                .admit(
                    Resources {
                        cpu_cores: 3,
                        ..before
                    },
                    quota
                )
                .is_ok()
        );
        assert!(
            before
                .admit(
                    Resources {
                        memory_bytes: 25,
                        ..before
                    },
                    quota
                )
                .is_err()
        );
        let after_delete = Resources {
            memory_bytes: 12,
            ..before
        };
        assert!(
            after_delete
                .admit(
                    Resources {
                        memory_bytes: 16,
                        ..before
                    },
                    quota
                )
                .is_ok()
        );
        assert!(
            after_delete
                .admit(
                    Resources {
                        memory_bytes: 20,
                        ..before
                    },
                    quota
                )
                .is_err()
        );
    }
}
