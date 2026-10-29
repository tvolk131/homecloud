use crate::{Lifecycle, Resources, VmSpec};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub fn canonical_gpu_set(mut pcis: Vec<String>) -> Vec<String> {
    for pci in &mut pcis {
        pci.make_ascii_lowercase();
    }
    pcis.sort();
    pcis.dedup();
    pcis
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "VmWire", into = "VmWire")]
pub struct Vm {
    pub id: Uuid,
    pub owner: Uuid,
    pub spec: VmSpec,
    /// Durable backing including pending work and unreleased old capacity.
    pub resources: Resources,
    pub cpu_ids: Vec<u32>,
    pub network_slot: u16,
    pub desired: Lifecycle,
    pub observed: String,
    pub ssh_host_key: Option<String>,
    /// Attached/claimed devices, retained through physical cleanup.
    pub gpus: Vec<String>,
    /// Next-start preferences; they convey no ownership while stopped.
    pub preferred_gpus: Vec<String>,
    /// Additional devices claimed while an edit is being provisioned.
    pub pending_gpus: Vec<String>,
    pub error: Option<String>,
}
impl Vm {
    pub fn claims_gpu(&self, pci: &str) -> bool {
        self.gpus
            .iter()
            .chain(&self.pending_gpus)
            .any(|held| held.eq_ignore_ascii_case(pci))
    }
    pub fn has_compute(&self) -> bool {
        self.resources.cpu_cores != 0
    }
    pub fn incus_name(&self) -> String {
        format!("hc-{}", self.id.simple())
    }
    pub fn bridge(&self) -> String {
        format!("hcvm{}", self.network_slot)
    }
    pub fn address(&self) -> String {
        format!("10.78.{}.10", self.network_slot)
    }
}

// Retain scalar mirrors for old stored records/readers during an upgrade.
// Collection fields are authoritative, including an explicit empty collection.
#[derive(Serialize, Deserialize)]
struct VmWire {
    id: Uuid,
    owner: Uuid,
    spec: VmSpec,
    resources: Resources,
    cpu_ids: Vec<u32>,
    network_slot: u16,
    desired: Lifecycle,
    observed: String,
    ssh_host_key: Option<String>,
    error: Option<String>,
    #[serde(default)]
    gpus: Option<Vec<String>>,
    #[serde(default)]
    preferred_gpus: Option<Vec<String>>,
    #[serde(default)]
    pending_gpus: Option<Vec<String>>,
    #[serde(default)]
    gpu: Option<String>,
    #[serde(default)]
    preferred_gpu: Option<String>,
    #[serde(default)]
    pending_gpu: Option<String>,
}
impl From<VmWire> for Vm {
    fn from(w: VmWire) -> Self {
        Self {
            id: w.id,
            owner: w.owner,
            spec: w.spec,
            resources: w.resources,
            cpu_ids: w.cpu_ids,
            network_slot: w.network_slot,
            desired: w.desired,
            observed: w.observed,
            ssh_host_key: w.ssh_host_key,
            error: w.error,
            gpus: canonical_gpu_set(w.gpus.unwrap_or_else(|| w.gpu.into_iter().collect())),
            preferred_gpus: canonical_gpu_set(
                w.preferred_gpus
                    .unwrap_or_else(|| w.preferred_gpu.into_iter().collect()),
            ),
            pending_gpus: canonical_gpu_set(
                w.pending_gpus
                    .unwrap_or_else(|| w.pending_gpu.into_iter().collect()),
            ),
        }
    }
}
impl From<Vm> for VmWire {
    fn from(v: Vm) -> Self {
        Self {
            id: v.id,
            owner: v.owner,
            spec: v.spec,
            resources: v.resources,
            cpu_ids: v.cpu_ids,
            network_slot: v.network_slot,
            desired: v.desired,
            observed: v.observed,
            ssh_host_key: v.ssh_host_key,
            error: v.error,
            gpu: v.gpus.first().cloned(),
            preferred_gpu: v.preferred_gpus.first().cloned(),
            pending_gpu: v.pending_gpus.first().cloned(),
            gpus: Some(v.gpus),
            preferred_gpus: Some(v.preferred_gpus),
            pending_gpus: Some(v.pending_gpus),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn legacy() -> serde_json::Value {
        serde_json::json!({
            "id":Uuid::new_v4(),"owner":Uuid::new_v4(),
            "spec":{"name":"old","memory_gib":4,"cores":1,"disk_gib":10,"ssh_public_key":"fixture","applications":[]},
            "resources":{"memory_bytes":4294967296u64,"cpu_cores":1,"disk_bytes":12884901888u64},
            "cpu_ids":[2,8],"network_slot":1,"desired":"running","observed":"running",
            "ssh_host_key":null,"error":null,"gpu":"0000:0A:00.0","preferred_gpu":"0000:0A:00.0","pending_gpu":null
        })
    }
    #[test]
    fn scalar_vm_records_upgrade_without_losing_a_claim_or_preference() {
        let vm: Vm = serde_json::from_value(legacy()).unwrap();
        assert_eq!(vm.gpus, vec!["0000:0a:00.0"]);
        assert_eq!(vm.preferred_gpus, vm.gpus);
        assert!(vm.claims_gpu("0000:0A:00.0"));
        assert!(vm.pending_gpus.is_empty());
    }
    #[test]
    fn multi_gpu_records_round_trip_every_claim_and_canonicalize_ids() {
        let mut record = legacy();
        record["gpus"] = serde_json::json!(["0000:0B:00.0", "0000:0a:00.0", "0000:0A:00.0"]);
        record["pending_gpus"] = serde_json::json!(["0000:0C:00.0"]);
        let vm: Vm = serde_json::from_value(record).unwrap();
        assert_eq!(vm.gpus, vec!["0000:0a:00.0", "0000:0b:00.0"]);
        let wire = serde_json::to_value(&vm).unwrap();
        assert_eq!(wire["gpu"], "0000:0a:00.0");
        let restored: Vm = serde_json::from_value(wire).unwrap();
        assert_eq!(restored.gpus, vm.gpus);
        assert_eq!(restored.pending_gpus, vm.pending_gpus);
        assert!(restored.claims_gpu("0000:0C:00.0"));
    }
    #[test]
    fn explicit_empty_collections_override_legacy_scalar_mirrors() {
        let mut record = legacy();
        record["gpus"] = serde_json::json!([]);
        record["preferred_gpus"] = serde_json::json!([]);
        let vm: Vm = serde_json::from_value(record).unwrap();
        assert!(vm.gpus.is_empty() && vm.preferred_gpus.is_empty());
    }
}
