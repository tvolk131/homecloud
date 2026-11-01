use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmMetricsState {
    #[default]
    NotConfigured,
    Waiting,
    Ready,
    Stale,
    Unavailable,
    Stopped,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VmMetrics {
    pub vm_id: Uuid,
    pub state: VmMetricsState,
    pub observed_at: Option<u64>,
    pub attempted_at: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub memory: Option<VmMemoryUsage>,
    pub root_filesystem: Option<VmFilesystemUsage>,
    pub network: Vec<VmNetworkUsage>,
}

impl VmMetrics {
    pub fn empty(vm_id: Uuid, state: VmMetricsState) -> Self {
        Self {
            vm_id,
            state,
            observed_at: None,
            attempted_at: None,
            cpu_percent: None,
            memory: None,
            root_filesystem: None,
            network: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmMemoryUsage {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

impl VmMemoryUsage {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmFilesystemUsage {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
}

impl VmFilesystemUsage {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VmNetworkUsage {
    pub interface: String,
    pub received_bytes: u64,
    pub sent_bytes: u64,
    pub receive_bytes_per_second: Option<f64>,
    pub send_bytes_per_second: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Snapshot;
    use serde_json::json;

    #[test]
    fn memory_used_is_total_minus_guest_available_and_saturates() {
        let mut memory = VmMemoryUsage {
            total_bytes: 1000,
            available_bytes: 600,
        };
        assert_eq!(memory.used_bytes(), 400);
        memory.available_bytes = 1001;
        assert_eq!(memory.used_bytes(), 0);
    }

    #[test]
    fn filesystem_used_excludes_reserved_free_blocks() {
        let mut filesystem = VmFilesystemUsage {
            total_bytes: 10000,
            free_bytes: 4000,
            available_bytes: 3500,
        };
        assert_eq!(filesystem.used_bytes(), 6000);
        assert_eq!(filesystem.free_bytes - filesystem.available_bytes, 500);
        assert_eq!(
            filesystem.used_bytes() + filesystem.free_bytes,
            filesystem.total_bytes
        );
        filesystem.available_bytes = 0;
        assert_eq!(filesystem.used_bytes(), 6000);
        filesystem.free_bytes = 10001;
        assert_eq!(filesystem.used_bytes(), 0);
    }

    #[test]
    fn empty_metrics_and_states_round_trip_without_invented_usage() {
        let vm_id = Uuid::new_v4();
        for state in [
            VmMetricsState::NotConfigured,
            VmMetricsState::Waiting,
            VmMetricsState::Ready,
            VmMetricsState::Stale,
            VmMetricsState::Unavailable,
            VmMetricsState::Stopped,
            VmMetricsState::Unknown,
        ] {
            let metrics = VmMetrics::empty(vm_id, state);
            assert_eq!(metrics.vm_id, vm_id);
            assert_eq!(metrics.state, state);
            assert_eq!(metrics.observed_at, None);
            assert_eq!(metrics.attempted_at, None);
            assert_eq!(metrics.cpu_percent, None);
            assert_eq!(metrics.memory, None);
            assert_eq!(metrics.root_filesystem, None);
            assert!(metrics.network.is_empty());
            let encoded = serde_json::to_value(&metrics).unwrap();
            assert!(
                encoded["state"]
                    .as_str()
                    .unwrap()
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character == '_')
            );
            assert_eq!(
                serde_json::from_value::<VmMetrics>(encoded).unwrap(),
                metrics
            );
        }
        assert_eq!(VmMetricsState::default(), VmMetricsState::NotConfigured);
        assert_eq!(
            serde_json::from_str::<VmMetricsState>("\"future_state\"").unwrap(),
            VmMetricsState::Unknown
        );
    }

    #[test]
    fn old_snapshot_without_metrics_remains_readable_and_omits_empty_metrics() {
        let snapshot = json!({
            "me": {
                "id": Uuid::new_v4(), "name": "Legacy member", "role": "user", "active": true,
                "grant": {"memory_bytes": 0, "cpu_cores": 0, "disk_bytes": 0},
                "committed": {"memory_bytes": 0, "cpu_cores": 0, "disk_bytes": 0},
                "gpu_allowed": false
            },
            "users": [], "devices": [], "vms": [], "operations": [],
            "inventory": {
                "hostname": "legacy-host", "cpu_model": "CPU", "physical_cores": 2,
                "memory_bytes": 1000, "pool": "pool", "pool_available_bytes": 10000,
                "pool_health": "ONLINE", "iommu": null, "gpus": [], "warnings": []
            },
            "capacity": {"memory_bytes": 1000, "cpu_cores": 2, "disk_bytes": 10000}
        });
        let mut decoded: Snapshot = serde_json::from_value(snapshot).unwrap();
        assert!(decoded.vm_metrics.is_empty());
        assert_eq!(decoded.disk_overhead_gib_per_vm, 2);
        assert!(
            serde_json::to_value(&decoded)
                .unwrap()
                .get("vm_metrics")
                .is_none()
        );
        let metrics = VmMetrics::empty(Uuid::new_v4(), VmMetricsState::Waiting);
        decoded.vm_metrics.push(metrics.clone());
        let mut encoded = serde_json::to_value(&decoded).unwrap();
        assert_eq!(encoded["vm_metrics"].as_array().unwrap().len(), 1);
        assert_eq!(
            serde_json::from_value::<Snapshot>(encoded.clone())
                .unwrap()
                .vm_metrics,
            vec![metrics]
        );
        encoded["vm_metrics"][0]["state"] = json!("future_state");
        assert_eq!(
            serde_json::from_value::<Snapshot>(encoded)
                .unwrap()
                .vm_metrics[0]
                .state,
            VmMetricsState::Unknown
        );
    }
}
