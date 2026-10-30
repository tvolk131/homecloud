use anyhow::Result;
use async_trait::async_trait;
use homecloud_core::*;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

pub mod observer;
mod process;
pub use process::{HOST_OBSERVER_PATH, HostCommand, ObservingBackend, ProcessBackend};

pub mod storage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub state: String,
    pub ssh_host_key: Option<String>,
    /// Explicit adapter confirmation after shutdown and GPU/CPU cleanup.
    /// Older adapters omit it and cannot authorize compute release.
    #[serde(default)]
    pub compute_released: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmObservation {
    #[serde(default)]
    pub progress: Option<VmSetupProgress>,
    pub id: uuid::Uuid,
    pub state: String,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostRequest {
    ShutdownStatus,
    CancelShutdown {
        operation_id: uuid::Uuid,
        boot_id: String,
    },
    CheckShutdownFence {
        operation_id: uuid::Uuid,
        boot_id: String,
    },
    CheckShutdown {
        boot_id: String,
        vms: Vec<Vm>,
    },
    DrainGuest {
        operation_id: uuid::Uuid,
        vm: Vm,
        boot_id: String,
    },
    PowerOff {
        operation_id: uuid::Uuid,
        boot_id: String,
        vms: Vec<Vm>,
    },
    Diagnostics {
        vm: Vm,
    },
    Application {
        vm: Vm,
        application: Box<ManagedApplication>,
        command: ApplicationCommand,
    },
    Inventory,
    Storage,
    CancelMemoryCapacity {
        operation: MemoryCapacityOperation,
    },
    AdoptMemoryCapacity {
        operation: MemoryCapacityOperation,
    },
    ReserveStatus,
    DiscardReserves {
        operation: HostReserveOperation,
    },
    ApplyReserves {
        operation: HostReserveOperation,
        vms: Vec<Vm>,
        users: Vec<User>,
    },
    Observe {
        vms: Vec<Vm>,
    },
    Backing {
        users: Vec<User>,
    },
    Apply {
        vm: Vm,
        vms: Vec<Vm>,
        users: Vec<User>,
        mutation: Mutation,
    },
    VerifySsh {
        vm: Vm,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HostResponse {
    PowerOffRejected {
        operation_id: uuid::Uuid,
        boot_id: String,
        message: String,
    },
    PowerOffQueued {
        operation_id: uuid::Uuid,
        boot_id: String,
    },
    ShutdownBoot(String),
    Diagnostics(VmDiagnostics),
    Application(ApplicationReport),
    Inventory(HostInventory),
    Storage(StorageInventory),
    MemoryCapacity(MemoryCapacityStatus),
    MemoryCapacityCancelled {
        operation_id: uuid::Uuid,
        status: MemoryCapacityStatus,
    },
    ReservePolicy(Option<HostReservePolicy>),
    DiscardedReserves {
        operation_id: uuid::Uuid,
        policy: HostReservePolicy,
    },
    Observations(Vec<VmObservation>),
    Applied(Observation),
    Ok,
    Error(String),
}

#[async_trait]
pub trait Backend: Send + Sync {
    async fn request(&self, request: HostRequest) -> Result<HostResponse>;
}

/// Persistent observed state for local development and crash-boundary tests.
pub struct Simulator {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}
impl Simulator {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }
}
#[async_trait]
impl Backend for Simulator {
    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        let _guard = self.lock.lock().await;
        match request {
            HostRequest::ShutdownStatus
            | HostRequest::CancelShutdown { .. }
            | HostRequest::CheckShutdownFence { .. }
            | HostRequest::CheckShutdown { .. }
            | HostRequest::DrainGuest { .. }
            | HostRequest::PowerOff { .. } => {
                anyhow::bail!("Host shutdown requires a capable Linux host")
            }
            HostRequest::CancelMemoryCapacity { .. } => {
                anyhow::bail!("Memory capacity recovery requires a real host")
            }
            HostRequest::AdoptMemoryCapacity { .. } => {
                anyhow::bail!("Memory capacity adoption requires a real host")
            }
            HostRequest::Diagnostics { .. } => {
                anyhow::bail!("Guest setup logs require a real Linux host")
            }
            HostRequest::Application { .. } => Ok(HostResponse::Application(ApplicationReport {
                observation: ApplicationObservation {
                    service: "unavailable".into(),
                    setup: "required".into(),
                    message: "Simulation does not execute guest applications".into(),
                    ..Default::default()
                },
                job: None,
            })),
            HostRequest::Observe { .. } => Ok(HostResponse::Observations(vec![])),
            HostRequest::Storage => Ok(HostResponse::Storage(StorageInventory {
                warnings: vec!["Simulation does not discover disks or collect SMART data.".into()],
                ..Default::default()
            })),
            HostRequest::Inventory => Ok(HostResponse::Inventory(HostInventory {
                hostname: "Simulator — local development".into(),
                cpu_model: "Simulated host".into(),
                physical_cores: 2,
                memory_bytes: 32 * GIB,
                pool: "simulation".into(),
                pool_available_bytes: 128 * GIB,
                pool_health: "simulated".into(),
                warnings: vec!["Simulation: these instances do not execute guest workloads".into()],
                ..Default::default()
            })),
            HostRequest::ReserveStatus => Ok(HostResponse::ReservePolicy(None)),
            HostRequest::DiscardReserves { .. } | HostRequest::ApplyReserves { .. } => {
                anyhow::bail!("Reserve enforcement requires a capable host backend")
            }
            HostRequest::Backing { .. } => Ok(HostResponse::Ok),
            HostRequest::VerifySsh { .. } => anyhow::bail!("SSH requires a real Linux host"),
            HostRequest::Apply { vm, .. } => {
                let mut observed: BTreeMap<String, serde_json::Value> = if self.path.exists() {
                    serde_json::from_slice(&std::fs::read(&self.path)?)?
                } else {
                    BTreeMap::new()
                };
                let state = match vm.desired {
                    Lifecycle::Running => "running",
                    Lifecycle::Stopped => "stopped",
                    Lifecycle::Deleted => "deleted",
                };
                observed.insert(
                    vm.id.to_string(),
                    serde_json::json!({"owner":vm.owner,"state":state}),
                );
                if let Some(parent) = self.path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let temporary = self.path.with_extension("tmp");
                std::fs::write(&temporary, serde_json::to_vec(&observed)?)?;
                std::fs::rename(temporary, &self.path)?;
                Ok(HostResponse::Applied(Observation {
                    state: state.into(),
                    ssh_host_key: None,
                    compute_released: vm.desired != Lifecycle::Running,
                }))
            }
        }
    }
}
