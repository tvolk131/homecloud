use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use homecloud_core::*;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::{io::AsyncWriteExt, process::Command, sync::Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub state: String,
    pub ssh_host_key: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmObservation {
    pub id: uuid::Uuid,
    pub state: String,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostRequest {
    Inventory,
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
    Inventory(HostInventory),
    Observations(Vec<VmObservation>),
    Applied(Observation),
    Ok,
    Error(String),
}

#[async_trait]
pub trait Backend: Send + Sync {
    async fn request(&self, request: HostRequest) -> Result<HostResponse>;
}

pub struct ProcessBackend {
    pub script: PathBuf,
    pub sudo: bool,
}
#[async_trait]
impl Backend for ProcessBackend {
    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        let mut command = if self.sudo {
            let mut c = Command::new("sudo");
            c.args(["-n", "--", "/usr/bin/python3"]);
            c
        } else {
            Command::new("/usr/bin/python3")
        };
        command
            .arg(&self.script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .context("Starting privileged host adapter")?;
        child
            .stdin
            .take()
            .context("Host adapter stdin")?
            .write_all(&serde_json::to_vec(&request)?)
            .await?;
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(240),
            child.wait_with_output(),
        )
        .await
        .context("Host adapter timed out; allocation remains held")??;
        ensure!(
            output.stdout.len() <= 4 * MAX_FRAME,
            "Host response too large"
        );
        ensure!(
            output.status.success(),
            "Host adapter: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: HostResponse =
            serde_json::from_slice(&output.stdout).context("Invalid host adapter response")?;
        if let HostResponse::Error(message) = response {
            anyhow::bail!("{message}");
        }
        Ok(response)
    }
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
            HostRequest::Observe { .. } => Ok(HostResponse::Observations(vec![])),
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
                }))
            }
        }
    }
}
