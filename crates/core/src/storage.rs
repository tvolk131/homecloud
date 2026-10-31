use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageCollection {
    pub attempted_at: Option<u64>,
    pub observed_at: Option<u64>,
    pub error: Option<String>,
}

impl StorageCollection {
    pub fn stale(&self, now: u64, maximum_age: u64) -> bool {
        self.error.is_some()
            || self
                .observed_at
                .is_none_or(|observed| observed > now || now.saturating_sub(observed) > maximum_age)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageInventory {
    pub disk_collection: StorageCollection,
    pub pool_collection: StorageCollection,
    pub disks: Vec<PhysicalDisk>,
    pub pools: Vec<StoragePool>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PhysicalDisk {
    pub id: String,
    pub path: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub wwn: Option<String>,
    pub transport: Option<String>,
    pub size_bytes: Option<u64>,
    pub rotational: Option<bool>,
    pub devices: Vec<StorageBlockDevice>,
    pub smart: SmartObservation,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageBlockDevice {
    pub path: String,
    pub filesystem: Option<String>,
    pub mountpoints: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartState {
    #[default]
    Pending,
    Current,
    Sleeping,
    Unsupported,
    Unavailable,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartObservation {
    pub collection: StorageCollection,
    pub state: SmartState,
    pub passed: Option<bool>,
    pub temperature_celsius: Option<i64>,
    pub power_on_hours: Option<u64>,
    pub metrics: Vec<SmartMetric>,
    pub self_tests: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SmartMetric {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StoragePool {
    pub name: String,
    pub guid: String,
    pub state: String,
    pub managed: bool,
    pub status: Option<String>,
    pub scan: Option<String>,
    pub vdevs: Vec<StorageVdev>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageVdev {
    pub guid: String,
    pub name: String,
    pub kind: String,
    pub role: String,
    pub state: String,
    pub path: Option<String>,
    pub disk_id: Option<String>,
    pub read_errors: Option<u64>,
    pub write_errors: Option<u64>,
    pub checksum_errors: Option<u64>,
    pub children: Vec<StorageVdev>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_and_failed_observations_are_not_fresh_or_healthy() {
        let mut collection = StorageCollection::default();
        assert!(collection.stale(100, 60));
        collection.observed_at = Some(80);
        assert!(!collection.stale(100, 60));
        assert!(collection.stale(150, 60));
        assert!(collection.stale(70, 60));
        collection.error = Some("Collection failed".into());
        assert!(collection.stale(100, 60));
        let smart = SmartObservation::default();
        assert_eq!(smart.state, SmartState::Pending);
        assert_eq!(smart.passed, None);
    }
}
