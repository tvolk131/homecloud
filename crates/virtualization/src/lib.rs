use anyhow::{Result, ensure};
use async_trait::async_trait;
use homecloud_core::*;
use serde::Deserialize;
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

mod resources;
pub use resources::parse_proxmox_resources;

#[cfg(all(target_os = "linux", feature = "proxmox"))]
pub mod proxmox;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxmoxConnection {
    pub endpoint: String,
    pub token_id: String,
    pub token_secret_file: PathBuf,
    pub tls_fingerprint: Option<String>,
}

pub fn connect_proxmox(config: ProxmoxConnection) -> Result<Arc<dyn VirtualizationBackend>> {
    #[cfg(all(target_os = "linux", feature = "proxmox"))]
    {
        Ok(Arc::new(proxmox::ProxmoxVirtualization::connect(config)?))
    }
    #[cfg(not(all(target_os = "linux", feature = "proxmox")))]
    {
        let _ = config;
        anyhow::bail!("The official Proxmox client requires a Linux build with --features proxmox")
    }
}

#[async_trait]
pub trait VirtualizationBackend: Send + Sync {
    fn identity(&self) -> Option<String> {
        None
    }
    async fn inventory(&self) -> Result<VirtualizationInventory>;
    async fn submit_power(
        &self,
        _: &VirtualizationGuestIdentity,
        _: VirtualizationPowerAction,
    ) -> PowerSubmission {
        PowerSubmission::Rejected("VM power controls are unavailable for this backend".into())
    }
    async fn power_task(
        &self,
        _: &VirtualizationGuestIdentity,
        _: VirtualizationPowerAction,
        _: &str,
    ) -> Result<PowerTaskState> {
        anyhow::bail!("VM power task tracking is unavailable for this backend")
    }
}

pub enum PowerSubmission {
    Accepted(String),
    Rejected(String),
    Unknown(String),
}

#[cfg(any(test, all(target_os = "linux", feature = "proxmox")))]
fn explicit_power_rejection(body: &[u8]) -> Option<u16> {
    let envelope: serde_json::Value = serde_json::from_slice(body).ok()?;
    let failed = matches!(
        envelope.get("success"),
        Some(serde_json::Value::Bool(false))
    ) || envelope.get("success").and_then(serde_json::Value::as_u64) == Some(0);
    let status = envelope.get("status")?.as_u64()?;
    (failed && (400..500).contains(&status) && status != 408).then_some(status as u16)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerTaskState {
    Running,
    Succeeded,
    Failed,
}

pub struct SimulatedVirtualization {
    inventory: RwLock<VirtualizationInventory>,
}

impl SimulatedVirtualization {
    pub fn new(inventory: VirtualizationInventory) -> Result<Self> {
        ensure!(
            inventory.source == VirtualizationSource::Simulated,
            "A simulated backend must be explicitly labeled simulated"
        );
        Ok(Self {
            inventory: RwLock::new(inventory),
        })
    }

    pub fn replace(&self, inventory: VirtualizationInventory) -> Result<()> {
        ensure!(
            inventory.source == VirtualizationSource::Simulated,
            "A simulated backend must be explicitly labeled simulated"
        );
        *self
            .inventory
            .write()
            .map_err(|_| anyhow::anyhow!("Simulated inventory lock unavailable"))? = inventory;
        Ok(())
    }

    pub fn demo() -> Self {
        Self::new(VirtualizationInventory {
            source: VirtualizationSource::Simulated,
            nodes: vec![VirtualizationNode {
                name: "demo-proxmox".into(),
                status: "online".into(),
                cpu_threads: Some(16),
                cpu_usage_ratio: Some(0.125),
                memory_capacity_bytes: Some(64 * GIB),
                memory_used_bytes: Some(12 * GIB),
            }],
            guests: vec![
                VirtualizationGuest {
                    node: "demo-proxmox".into(),
                    vmid: 101,
                    kind: VirtualizationGuestKind::VirtualMachine,
                    name: Some("t3-workstation".into()),
                    status: "running".into(),
                    template: false,
                    cpu_limit: Some(4),
                    memory_limit_bytes: Some(8 * GIB),
                    memory_used_bytes: Some(3 * GIB),
                    reported_disk_capacity_bytes: Some(64 * GIB),
                },
                VirtualizationGuest {
                    node: "demo-proxmox".into(),
                    vmid: 9000,
                    kind: VirtualizationGuestKind::VirtualMachine,
                    name: Some("clean-linux-template".into()),
                    status: "stopped".into(),
                    template: true,
                    cpu_limit: Some(2),
                    memory_limit_bytes: Some(2 * GIB),
                    memory_used_bytes: None,
                    reported_disk_capacity_bytes: Some(16 * GIB),
                },
            ],
            storage: vec![VirtualizationStorage {
                node: "demo-proxmox".into(),
                name: "local-zfs".into(),
                status: "available".into(),
                capacity_bytes: Some(1024 * GIB),
                used_bytes: Some(80 * GIB),
            }],
        })
        .expect("Demo inventory is explicitly simulated")
    }
}

#[async_trait]
impl VirtualizationBackend for SimulatedVirtualization {
    fn identity(&self) -> Option<String> {
        Some("simulated".into())
    }
    async fn inventory(&self) -> Result<VirtualizationInventory> {
        self.inventory
            .read()
            .map(|inventory| inventory.clone())
            .map_err(|_| anyhow::anyhow!("Simulated inventory lock unavailable"))
    }

    async fn submit_power(
        &self,
        identity: &VirtualizationGuestIdentity,
        action: VirtualizationPowerAction,
    ) -> PowerSubmission {
        let outcome = (|| -> Result<String> {
            identity.validate()?;
            let mut inventory = self
                .inventory
                .write()
                .map_err(|_| anyhow::anyhow!("Simulated inventory lock unavailable"))?;
            let guest = inventory
                .guests
                .iter_mut()
                .find(|guest| {
                    guest.node == identity.node
                        && guest.vmid == identity.vmid
                        && guest.kind == identity.kind
                })
                .ok_or_else(|| anyhow::anyhow!("Guest is no longer visible"))?;
            ensure!(!guest.template, "Templates cannot be powered on or off");
            guest.status = if action == VirtualizationPowerAction::Start {
                "running"
            } else {
                "stopped"
            }
            .into();
            Ok(format!(
                "simulated:{}:{}:{action:?}",
                identity.node, identity.vmid
            ))
        })();
        match outcome {
            Ok(task) => PowerSubmission::Accepted(task),
            Err(error) => PowerSubmission::Rejected(error.to_string()),
        }
    }

    async fn power_task(
        &self,
        identity: &VirtualizationGuestIdentity,
        action: VirtualizationPowerAction,
        task: &str,
    ) -> Result<PowerTaskState> {
        ensure!(
            task == format!("simulated:{}:{}:{action:?}", identity.node, identity.vmid),
            "Invalid simulated task"
        );
        Ok(PowerTaskState::Succeeded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_client_rejection_envelopes_release_a_power_request() {
        for body in [
            serde_json::json!({"success": 0, "status": 403}),
            serde_json::json!({"success": false, "status": 403}),
        ] {
            assert_eq!(
                explicit_power_rejection(&serde_json::to_vec(&body).unwrap()),
                Some(403)
            );
        }
        for body in [
            serde_json::json!({"data": "UPID:accepted-task"}),
            serde_json::json!({"success": 0}),
            serde_json::json!({"success": false}),
            serde_json::json!({"status": 403}),
            serde_json::json!({"success": 1, "status": 403}),
            serde_json::json!({"success": 0, "status": 408}),
            serde_json::json!({"success": 0, "status": 500}),
        ] {
            assert_eq!(
                explicit_power_rejection(&serde_json::to_vec(&body).unwrap()),
                None
            );
        }
        assert_eq!(explicit_power_rejection(b"not JSON"), None);
    }

    #[test]
    #[cfg(not(all(target_os = "linux", feature = "proxmox")))]
    fn unavailable_official_client_never_falls_back_to_simulation() {
        assert!(
            connect_proxmox(ProxmoxConnection {
                endpoint: "https://pve:8006/".into(),
                token_id: "reader@pve!homecloud".into(),
                token_secret_file: "not-read-on-this-platform".into(),
                tls_fingerprint: None,
            })
            .is_err()
        );
    }

    #[tokio::test]
    async fn demo_is_deterministic_and_can_be_updated_without_guest_operations() {
        let backend = SimulatedVirtualization::demo();
        let mut inventory = backend.inventory().await.unwrap();
        assert_eq!(inventory, backend.inventory().await.unwrap());
        assert_eq!(inventory.source, VirtualizationSource::Simulated);
        inventory.guests[0].status = "stopped".into();
        backend.replace(inventory.clone()).unwrap();
        assert_eq!(inventory, backend.inventory().await.unwrap());
        inventory.source = VirtualizationSource::Proxmox;
        assert!(backend.replace(inventory).is_err());
        assert_eq!(
            backend.inventory().await.unwrap().source,
            VirtualizationSource::Simulated
        );
    }
}
