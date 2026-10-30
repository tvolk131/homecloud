//! Fixed-path, read-only Linux adapter. Mutations remain in the primary backend.
use crate::{HostRequest, HostResponse, VmObservation};
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use homecloud_core::{Lifecycle, Vm, VmSetupProgress};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

mod inventory;
#[cfg(test)]
mod tests;

const MAX_REQUEST: u64 = 1024 * 1024;
const MAX_OUTPUT: usize = 4 * homecloud_core::MAX_FRAME;
type Devices = BTreeMap<String, BTreeMap<String, String>>;

#[derive(Debug, Clone, Deserialize)]
struct Instance {
    name: String,
    status: String,
    config: BTreeMap<String, String>,
    devices: Devices,
    #[serde(default)]
    expanded_devices: Option<Devices>,
}

struct CommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

#[async_trait]
trait Commands: Send + Sync {
    async fn run(&self, program: &str, args: &[&str], seconds: u64) -> Result<CommandOutput>;
}

struct SystemCommands;

async fn read_output(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_OUTPUT as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(bytes.len() <= MAX_OUTPUT, "Command response too large");
    Ok(bytes)
}

#[async_trait]
impl Commands for SystemCommands {
    async fn run(&self, program: &str, args: &[&str], seconds: u64) -> Result<CommandOutput> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("Starting {program}"))?;
        let stdout = child.stdout.take().context("Command stdout")?;
        let stderr = child.stderr.take().context("Command stderr")?;
        let (status, stdout, stderr) = tokio::time::timeout(Duration::from_secs(seconds), async {
            tokio::try_join!(
                async { Ok::<_, anyhow::Error>(child.wait().await?) },
                read_output(stdout),
                read_output(stderr)
            )
        })
        .await
        .with_context(|| format!("{program} timed out"))??;
        Ok(CommandOutput {
            success: status.success(),
            stdout: String::from_utf8(stdout)?,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

struct Observer<C> {
    root: PathBuf,
    commands: C,
}

impl<C: Commands> Observer<C> {
    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn read(&self, relative: &str) -> Result<String> {
        read(&self.path(relative))
    }

    async fn checked(&self, program: &str, args: &[&str], seconds: u64) -> Result<String> {
        let output = self.commands.run(program, args, seconds).await?;
        ensure!(
            output.success,
            "{program}: {}",
            if output.stderr.trim().is_empty() {
                output.stdout.trim()
            } else {
                output.stderr.trim()
            }
        );
        Ok(output.stdout)
    }

    async fn instances(&self) -> Result<Vec<Instance>> {
        serde_json::from_str(
            &self
                .checked(
                    "/usr/bin/incus",
                    &["query", "-X", "GET", "/1.0/instances?recursion=1"],
                    8,
                )
                .await?,
        )
        .context("Invalid Incus instance inventory")
    }

    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        match request {
            HostRequest::Inventory => Ok(HostResponse::Inventory(self.inventory().await?)),
            HostRequest::Observe { vms } => {
                Ok(HostResponse::Observations(self.observe(vms).await?))
            }
            _ => bail!("The host observer only supports inventory and observe"),
        }
    }

    async fn observe(&self, vms: Vec<Vm>) -> Result<Vec<VmObservation>> {
        let vms: Vec<_> = vms
            .into_iter()
            .filter(|vm| vm.observed != "deleted")
            .collect();
        let actual = match self.instances().await {
            Ok(instances) => instances
                .into_iter()
                .map(|v| (v.name.clone(), v))
                .collect::<BTreeMap<_, _>>(),
            Err(error) => {
                return Ok(vms
                    .into_iter()
                    .map(|vm| VmObservation {
                        id: vm.id,
                        state: "unavailable".into(),
                        error: Some(format!(
                            "Incus observation unavailable; allocations remain held: {error:#}"
                        )),
                        progress: None,
                    })
                    .collect());
            }
        };
        vms.into_iter().map(|vm| {
            let (state, error) = match actual.get(&vm.incus_name()) {
                Some(instance) => observe_instance(&vm, instance),
                None => ("missing".into(), Some("Recorded VM is missing from Incus; resources remain held for inspection".into())),
            };
            let path = self.path(&format!("var/lib/homecloud/host/setup-{}.json", vm.id.simple()));
            let progress: Option<VmSetupProgress> = match std::fs::read(&path) {
                Ok(data) => Some(serde_json::from_slice(&data).context("Invalid VM setup progress")?),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error).context("Reading VM setup progress"),
            };
            Ok(VmObservation { id: vm.id, state, error, progress })
        }).collect()
    }
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("Reading {}", path.display()))
}

fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}

fn field<'a>(values: &'a BTreeMap<String, String>, name: &str) -> &'a str {
    values.get(name).map(String::as_str).unwrap_or("")
}

fn matching_gpus(vm: &Vm, instance: &Instance) -> bool {
    let gpus = |devices: &Devices| {
        devices
            .iter()
            .filter(|(_, dev)| field(dev, "type") == "gpu")
            .map(|(name, dev)| (name.clone(), dev.clone()))
            .collect::<Devices>()
    };
    let devices = gpus(&instance.devices);
    for (name, device) in &devices {
        let Some(number) = name.strip_prefix("gpu") else {
            return false;
        };
        if number.is_empty()
            || !number.bytes().all(|b| b.is_ascii_digit())
            || field(device, "pci").is_empty()
            || device.get("gputype").is_some_and(|kind| kind != "physical")
        {
            return false;
        }
    }
    let attached: BTreeSet<_> = devices
        .values()
        .map(|dev| field(dev, "pci").to_ascii_lowercase())
        .collect();
    attached.len() == devices.len()
        && devices
            == gpus(
                instance
                    .expanded_devices
                    .as_ref()
                    .unwrap_or(&instance.devices),
            )
        && attached.iter().all(|pci| vm.claims_gpu(pci))
        && attached == vm.gpus.iter().cloned().collect()
}

fn observe_instance(vm: &Vm, instance: &Instance) -> (String, Option<String>) {
    let state = instance.status.to_ascii_lowercase();
    let config = &instance.config;
    let cpus = vm
        .cpu_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|cpu| cpu.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let desired = match vm.desired {
        Lifecycle::Running => "running",
        Lifecycle::Stopped => "stopped",
        Lifecycle::Deleted => "deleted",
    };
    let error = if field(config, "user.homecloud.id") != vm.id.to_string()
        || field(config, "user.homecloud.owner") != vm.owner.to_string()
    {
        Some("Ownership drift: operator review required")
    } else if (vm.cpu_ids.is_empty() && field(config, "boot.autostart") != "false")
        || (!vm.cpu_ids.is_empty() && field(config, "limits.cpu") != cpus)
        || field(config, "limits.memory") != format!("{}GiB", vm.spec.memory_gib)
        || instance.devices.get("root").map(|dev| field(dev, "size"))
            != Some(format!("{}GiB", vm.spec.disk_gib).as_str())
        || !matching_gpus(vm, instance)
    {
        Some("Configuration drift: operator review required")
    } else if state != desired {
        Some("Lifecycle differs from requested state; inspect before restarting")
    } else {
        None
    };
    (state, error.map(String::from))
}

/// Serve one fixed request. No CLI or request field can redirect host paths.
pub async fn serve() -> Result<HostResponse> {
    ensure!(
        cfg!(target_os = "linux"),
        "The host observer requires Linux"
    );
    ensure!(
        nix::unistd::geteuid().is_root(),
        "Run through the installed privileged adapter"
    );
    let mut input = Vec::new();
    tokio::io::stdin()
        .take(MAX_REQUEST + 1)
        .read_to_end(&mut input)
        .await?;
    ensure!(
        input.len() as u64 <= MAX_REQUEST,
        "Host request exceeds limit"
    );
    let request = serde_json::from_slice(&input).context("Invalid host request")?;
    Observer {
        root: "/".into(),
        commands: SystemCommands,
    }
    .request(request)
    .await
}
