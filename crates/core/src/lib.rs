use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CONTROL_ALPN: &[u8] = b"homecloud/control/1";
pub const SSH_ALPN: &[u8] = b"homecloud/ssh/1";
pub const MAX_FRAME: usize = 1024 * 1024;
pub const GIB: u64 = 1024 * 1024 * 1024;

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
    pub name: String,
    pub memory_gib: u64,
    pub cores: u64,
    pub disk_gib: u64,
    pub ssh_public_key: String,
}
impl VmSpec {
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
        validate_ssh_key(&self.ssh_public_key)?;
        Ok(Resources {
            memory_bytes: self.memory_gib * GIB,
            cpu_cores: self.cores,
            disk_bytes: self
                .disk_gib
                .checked_add(overhead_gib)
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
pub struct Vm {
    pub id: Uuid,
    pub owner: Uuid,
    pub spec: VmSpec,
    pub resources: Resources,
    pub cpu_ids: Vec<u32>,
    pub network_slot: u16,
    pub desired: Lifecycle,
    pub observed: String,
    pub ssh_host_key: Option<String>,
    pub gpu: Option<String>,
    pub error: Option<String>,
}
impl Vm {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
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
    pub hostname: String,
    pub cpu_model: String,
    pub physical_cores: usize,
    pub memory_bytes: u64,
    pub pool: String,
    pub pool_available_bytes: u64,
    pub pool_health: String,
    pub iommu: Option<Readiness>,
    pub gpus: Vec<Gpu>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub me: User,
    pub users: Vec<User>,
    pub devices: Vec<Device>,
    pub vms: Vec<Vm>,
    pub operations: Vec<Operation>,
    pub inventory: HostInventory,
    pub capacity: Resources,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Enroll {
        token: String,
    },
    Snapshot,
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
    Mutate {
        idempotency_key: Uuid,
        mutation: Mutation,
    },
    RetryOperation {
        operation_id: Uuid,
    },
    Ssh {
        vm_id: Uuid,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Response {
    Enrolled(User),
    Snapshot(Box<Snapshot>),
    Invitation {
        token: String,
        user_id: Uuid,
        expires_at: u64,
    },
    Operation(Operation),
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
