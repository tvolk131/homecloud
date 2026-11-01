use super::{Commands, Instance, Observer, entries, field, read};
use anyhow::{Context, Result, ensure};
use homecloud_core::{Gpu, HostInventory, Readiness, Resources};
use serde::Deserialize;
use std::{collections::BTreeSet, os::unix::fs::MetadataExt, path::Path};

#[derive(Deserialize)]
pub(super) struct Policy {
    pub pool: String,
    pub minimum_pool_available_bytes: u64,
    pub capacity: Resources,
    #[serde(default)]
    pub allowed_gpus: Vec<String>,
    #[serde(default)]
    pub allow_host_display_handoff: bool,
}

#[derive(Deserialize)]
struct Reserves {
    phase: String,
    state: ReserveState,
}

#[derive(Deserialize)]
struct ReserveState {
    additional: Resources,
}

fn root_owned(path: &Path, private: bool) -> Result<()> {
    let metadata = std::fs::metadata(path)?;
    let forbidden = if private { 0o077 } else { 0o022 };
    ensure!(
        metadata.is_file() && metadata.uid() == 0 && metadata.mode() & forbidden == 0,
        "{} must be {}root-owned regular file",
        path.display(),
        if private {
            "a private, "
        } else {
            "an exclusively root-writable, "
        }
    );
    Ok(())
}

impl Policy {
    fn apply_reserves(&mut self, reserves: Reserves) -> Result<()> {
        ensure!(
            matches!(reserves.phase.as_str(), "applied" | "applying"),
            "Invalid reserve journal phase"
        );
        let additional = reserves.state.additional;
        // Only disk affects inventory. RAM adoption may have increased the
        // effective memory envelope beyond the original install-time policy.
        ensure!(
            additional.disk_bytes <= self.capacity.disk_bytes,
            "Reserve exceeds reviewed capacity"
        );
        self.minimum_pool_available_bytes = self
            .minimum_pool_available_bytes
            .checked_add(additional.disk_bytes)
            .context("Pool reserve overflow")?;
        Ok(())
    }
}

impl<C: Commands> Observer<C> {
    fn policy(&self) -> Result<Policy> {
        let path = self.path("etc/homecloud/host.json");
        root_owned(&path, false)?;
        let mut policy: Policy = serde_json::from_str(&read(&path)?)?;
        ensure!(
            !policy.pool.is_empty()
                && policy
                    .pool
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Invalid pool name"
        );
        let journal = self.path("var/lib/homecloud/host/resource-reserves.json");
        match std::fs::metadata(&journal) {
            Ok(_) => {
                root_owned(&journal, true)?;
                policy.apply_reserves(serde_json::from_str(&read(&journal)?)?)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(policy)
    }

    pub(super) async fn inventory(&self) -> Result<HostInventory> {
        self.inventory_with_policy(self.policy()?).await
    }

    pub(super) async fn inventory_with_policy(&self, policy: Policy) -> Result<HostInventory> {
        let cpu = self.read("proc/cpuinfo")?;
        let memory = self.read("proc/meminfo")?;
        let mut groups = BTreeSet::new();
        for path in entries(&self.path("sys/devices/system/cpu"))? {
            let name = path.file_name().unwrap().to_string_lossy();
            if name
                .strip_prefix("cpu")
                .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                && path.join("topology/thread_siblings_list").exists()
            {
                groups.insert(
                    read(&path.join("topology/thread_siblings_list"))?
                        .trim()
                        .to_owned(),
                );
            }
        }
        let dmar = self.path("sys/firmware/acpi/tables/DMAR").exists();
        let ivrs = self.path("sys/firmware/acpi/tables/IVRS").exists();
        let count = entries(&self.path("sys/kernel/iommu_groups"))?.len();
        let ready = count > 0;
        let board = self.read("sys/class/dmi/id/board_name")?;
        let python_bool = |value| if value { "True" } else { "False" };
        let iommu = Readiness {
            state: if ready {
                "ready"
            } else {
                "needs_firmware_configuration"
            }
            .into(),
            evidence: vec![
                format!(
                    "DMAR={}; IVRS={}; IOMMU groups={count}",
                    python_bool(dmar),
                    python_bool(ivrs)
                ),
                format!("Motherboard: {}", board.trim()),
            ],
            next_steps: if ready {
                vec![]
            } else {
                vec![
                "Enable Intel VT-d or AMD IOMMU in firmware, then reboot and recheck.".into(),
                "If already enabled, inspect kernel IOMMU configuration. Missing groups alone do not prove unsupported hardware.".into(),
            ]
            },
        };
        let mut warnings = vec![];
        let instances = match self.instances().await {
            Ok(instances) => instances,
            Err(error) => {
                warnings.push(format!("Incus inventory unavailable: {error:#}"));
                vec![]
            }
        };
        let mut gpus = vec![];
        for path in entries(&self.path("sys/bus/pci/devices"))? {
            let class = read(&path.join("class"))?;
            if !class.trim().starts_with("0x0300") && !class.trim().starts_with("0x0302") {
                continue;
            }
            let pci = path.file_name().unwrap().to_string_lossy().into_owned();
            let iommu_path = path.join("iommu_group");
            let group = if iommu_path.exists() {
                Some(
                    std::fs::canonicalize(&iommu_path)?
                        .file_name()
                        .context("Missing IOMMU group")?
                        .to_string_lossy()
                        .parse::<u32>()?,
                )
            } else {
                None
            };
            let display =
                path.join("boot_vga").exists() && read(&path.join("boot_vga"))?.trim() == "1";
            let offered = policy.allowed_gpus.contains(&pci);
            let mut state = if !offered {
                "reserved_by_operator"
            } else if ready {
                "ready"
            } else {
                "needs_firmware_configuration"
            };
            if display && !policy.allow_host_display_handoff {
                state = "host_display_requires_policy";
            }
            let mut members = vec![];
            if group.is_some() {
                for member in entries(&iommu_path.join("devices"))? {
                    let name = member.file_name().unwrap().to_string_lossy().into_owned();
                    if !name.starts_with(&pci[..pci.len() - 1])
                        && !read(&member.join("class"))?.trim().starts_with("0x0604")
                    {
                        state = "unsafe_iommu_group";
                    }
                    members.push(name);
                }
            } else {
                state = "needs_firmware_configuration";
            }
            let owner = instances.iter().find(|instance| {
                instance
                    .expanded_devices
                    .as_ref()
                    .unwrap_or(&instance.devices)
                    .values()
                    .any(|dev| field(dev, "type") == "gpu" && field(dev, "pci") == pci)
            });
            let mut assigned_vm = None;
            let mut telemetry =
                "Unavailable: GPU is on the host or not responding to a supported guest query."
                    .to_owned();
            if let Some(owner) = owner {
                state = "assigned";
                assigned_vm = field(&owner.config, "user.homecloud.id").parse().ok();
                telemetry = self.telemetry(owner).await?;
            }
            let model = self
                .commands
                .run("/usr/bin/lspci", &["-s", &pci], 90)
                .await?
                .stdout
                .trim()
                .to_owned();
            let driver = if path.join("driver").exists() {
                Some(
                    std::fs::canonicalize(path.join("driver"))?
                        .file_name()
                        .context("Missing PCI driver name")?
                        .to_string_lossy()
                        .into_owned(),
                )
            } else {
                None
            };
            gpus.push(Gpu {
                pci,
                model,
                driver,
                iommu_group: group,
                host_display: display,
                assigned_vm,
                telemetry: Some(telemetry),
                readiness: Readiness {
                    state: state.into(),
                    evidence: vec![
                        format!(
                            "IOMMU group {}",
                            group.map_or("None".into(), |n| n.to_string())
                        ),
                        format!("Members: {}", members.join(", ")),
                        format!("Host display: {}", python_bool(display)),
                    ],
                    next_steps: if matches!(state, "ready" | "assigned") {
                        vec![]
                    } else {
                        vec![
                            "Review firmware, group membership, and operator assignment policy."
                                .into(),
                        ]
                    },
                },
            });
        }
        let pool = async {
            let available = self
                .checked(
                    "/usr/sbin/zfs",
                    &["get", "-Hp", "-o", "value", "available", &policy.pool],
                    90,
                )
                .await?
                .trim()
                .parse::<u64>()?;
            let health = self
                .checked(
                    "/usr/sbin/zpool",
                    &["list", "-H", "-o", "health", &policy.pool],
                    90,
                )
                .await?;
            Ok::<_, anyhow::Error>((available, health.trim().to_owned()))
        }
        .await;
        let (available, health) = match pool {
            Ok(pool) => pool,
            Err(error) => {
                warnings.push(format!("ZFS inventory unavailable: {error:#}"));
                (0, "unavailable".into())
            }
        };
        if available < policy.minimum_pool_available_bytes {
            warnings
                .push("Pool accounting headroom below operating floor; growth is blocked".into());
        }
        let memory_kib: u64 = memory
            .lines()
            .find_map(|line| line.strip_prefix("MemTotal:"))
            .and_then(|line| line.split_whitespace().next())
            .context("Missing MemTotal")?
            .parse()?;
        Ok(HostInventory {
            shutdown_boot_id: Some(
                uuid::Uuid::parse_str(self.read("proc/sys/kernel/random/boot_id")?.trim())
                    .context("Invalid host boot ID")?
                    .to_string(),
            ),
            // Storage is collected separately and attached by the daemon.
            storage: None,
            hostname: self.read("proc/sys/kernel/hostname")?.trim().into(),
            cpu_model: cpu
                .lines()
                .find_map(|line| {
                    line.strip_prefix("model name")
                        .and_then(|line| line.split_once(':'))
                        .map(|(_, value)| value.trim())
                })
                .unwrap_or("unknown")
                .into(),
            physical_cores: groups.len(),
            memory_bytes: memory_kib
                .checked_mul(1024)
                .context("Physical memory overflow")?,
            pool: policy.pool,
            pool_available_bytes: available,
            minimum_pool_available_bytes: Some(policy.minimum_pool_available_bytes),
            pool_health: health,
            iommu: Some(iommu),
            gpus,
            warnings,
        })
    }

    async fn telemetry(&self, owner: &Instance) -> Result<String> {
        let devices = owner.expanded_devices.as_ref().unwrap_or(&owner.devices);
        if devices
            .values()
            .filter(|dev| field(dev, "type") == "gpu")
            .count()
            > 1
        {
            return Ok("Per-device guest telemetry is unavailable for multi-GPU VMs.".into());
        }
        let unavailable =
            "Unavailable while the assigned VM is stopped or its guest driver is unavailable.";
        if owner.status != "Running" {
            return Ok(unavailable.into());
        }
        // Guest telemetry is optional; a timeout never changes ownership/readiness.
        let output = self
            .commands
            .run(
                "/usr/bin/incus",
                &[
                    "exec",
                    &owner.name,
                    "--",
                    "nvidia-smi",
                    "--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu",
                    "--format=csv,noheader,nounits",
                ],
                4,
            )
            .await;
        if let Ok(output) = output
            && output.success
            && let Some(summary) = telemetry_summary(&output.stdout)
        {
            return Ok(summary);
        }
        Ok(unavailable.into())
    }
}

// The guest controls this output. Keep a malformed sample local to telemetry
// rather than allowing it to enlarge or inject text into the shared inventory.
fn telemetry_summary(output: &str) -> Option<String> {
    if output.len() > 128 {
        return None;
    }
    let values: Vec<_> = output.trim().split(',').map(str::trim).collect();
    if values.len() != 4 || values.iter().any(|value| value.len() > 16) {
        return None;
    }
    let utilization: f64 = values[0].parse().ok()?;
    let used: u64 = values[1].parse().ok()?;
    let total: u64 = values[2].parse().ok()?;
    let temperature: f64 = values[3].parse().ok()?;
    if !utilization.is_finite()
        || !(0.0..=100.0).contains(&utilization)
        || !temperature.is_finite()
        || !(-100.0..=200.0).contains(&temperature)
        || used > total
    {
        return None;
    }
    Some(format!(
        "Guest GPU responding: {utilization}% utilization; {used}/{total} MiB VRAM; {temperature} C. Guest-reported, not an independent hardware health guarantee."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_floor_includes_durable_reserves_even_while_applying() {
        let mut policy: Policy = serde_json::from_value(serde_json::json!({
            "pool":"test", "minimum_pool_available_bytes":2,
            "capacity":{"memory_bytes":100,"cpu_cores":4,"disk_bytes":100}
        }))
        .unwrap();
        let record = |disk| {
            serde_json::from_value(serde_json::json!({
            "phase":"applying", "state":{"additional":{"memory_bytes":0,"cpu_cores":0,"disk_bytes":disk}}
        })).unwrap()
        };
        policy.apply_reserves(record(3)).unwrap();
        assert_eq!(policy.minimum_pool_available_bytes, 5);
        assert!(policy.apply_reserves(record(101)).is_err());
        policy.minimum_pool_available_bytes = u64::MAX;
        assert!(policy.apply_reserves(record(1)).is_err());
    }

    #[test]
    fn world_writable_policy_and_nonprivate_journal_are_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("policy.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(root_owned(&path, false).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(root_owned(&path, true).is_err());
        if !nix::unistd::geteuid().is_root() {
            assert!(root_owned(&path, false).is_err());
        }
    }
}
