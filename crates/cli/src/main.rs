use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use homecloud_client::{Client, default_profile};
use homecloud_core::*;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[derive(Parser)]
#[command(about = "Homecloud management and OpenSSH transport")]
struct Args {
    #[arg(long, global = true)]
    profile: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Read a one-time invitation from stdin, never command-line history.
    Enroll {
        #[arg(long)]
        ticket: PathBuf,
    },
    Status,
    Invite {
        name: String,
        #[arg(long)]
        admin: bool,
        #[arg(long, default_value_t = 0)]
        memory: u64,
        #[arg(long, default_value_t = 0)]
        cores: u64,
        #[arg(long, default_value_t = 0)]
        disk: u64,
        #[arg(long)]
        gpu: bool,
    },
    InviteDevice {
        user: Uuid,
    },
    Grant {
        user: Uuid,
        #[arg(long)]
        memory: u64,
        #[arg(long)]
        cores: u64,
        #[arg(long)]
        disk: u64,
        #[arg(long)]
        gpu: bool,
    },
    Revoke {
        peer: String,
    },
    Disable {
        user: Uuid,
    },
    Enable {
        user: Uuid,
    },
    Create {
        name: String,
        #[arg(long, default_value_t = 4)]
        memory: u64,
        #[arg(long, default_value_t = 1)]
        cores: u64,
        #[arg(long, default_value_t = 20)]
        disk: u64,
        #[arg(long)]
        ssh_key: PathBuf,
        #[arg(long)]
        request_id: Option<Uuid>,
    },
    Start {
        vm: Uuid,
    },
    Stop {
        vm: Uuid,
    },
    Delete {
        vm: Uuid,
    },
    Resize {
        vm: Uuid,
        #[arg(long)]
        memory: Option<u64>,
        #[arg(long)]
        cores: Option<u64>,
        #[arg(long)]
        disk: Option<u64>,
    },
    Gpu {
        vm: Uuid,
        #[arg(long, conflicts_with = "detach")]
        pci: Option<String>,
        #[arg(long)]
        detach: bool,
    },
    Wait {
        operation: Uuid,
        #[arg(long, default_value_t = 600)]
        seconds: u64,
    },
    Retry {
        operation: Uuid,
    },
    /// Binary-clean ProxyCommand. All diagnostics go to stderr.
    SshProxy {
        vm: Uuid,
    },
    /// Export pinned host-key trust; --install adds a managed SSH Include.
    SshConfig {
        vm: Uuid,
        #[arg(long)]
        install: bool,
    },
}
fn resources(memory: u64, cores: u64, disk: u64) -> Result<Resources> {
    Ok(Resources {
        memory_bytes: memory.checked_mul(GIB).context("Memory overflow")?,
        cpu_cores: cores,
        disk_bytes: disk.checked_mul(GIB).context("Disk overflow")?,
    })
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let profile = args.profile.unwrap_or_else(default_profile);
    if let Command::Enroll { ticket } = args.command {
        let server = serde_json::from_slice(&std::fs::read(ticket)?)?;
        eprint!("Invitation code: ");
        let mut token = String::new();
        std::io::stdin().read_line(&mut token)?;
        let (_, user) = Client::enroll(&profile, server, token.trim().into()).await?;
        println!("{}", serde_json::to_string_pretty(&user)?);
        return Ok(());
    }
    let client = Client::load(&profile).await?;
    let request = match args.command {
        Command::Status => Request::Snapshot,
        Command::Retry { operation } => Request::RetryOperation {
            operation_id: operation,
        },
        Command::Invite {
            name,
            admin,
            memory,
            cores,
            disk,
            gpu,
        } => Request::Invite {
            name,
            role: if admin { Role::Admin } else { Role::User },
            grant: resources(memory, cores, disk)?,
            gpu_allowed: gpu,
            expires_hours: 24,
        },
        Command::InviteDevice { user } => Request::InviteDevice {
            user_id: user,
            expires_hours: 24,
        },
        Command::Grant {
            user,
            memory,
            cores,
            disk,
            gpu,
        } => Request::Grant {
            user_id: user,
            grant: resources(memory, cores, disk)?,
            gpu_allowed: gpu,
        },
        Command::Revoke { peer } => Request::RevokeDevice { peer },
        Command::Disable { user } => Request::SetAccountActive {
            user_id: user,
            active: false,
        },
        Command::Enable { user } => Request::SetAccountActive {
            user_id: user,
            active: true,
        },
        Command::Create {
            name,
            memory,
            cores,
            disk,
            ssh_key,
            request_id,
        } => Request::Mutate {
            idempotency_key: request_id.unwrap_or_else(Uuid::new_v4),
            mutation: Mutation::Create {
                spec: VmSpec {
                    name,
                    memory_gib: memory,
                    cores,
                    disk_gib: disk,
                    ssh_public_key: std::fs::read_to_string(ssh_key)?.trim().into(),
                },
            },
        },
        Command::Start { vm } | Command::Stop { vm } | Command::Delete { vm } => {
            let desired = match args.command {
                Command::Start { .. } => Lifecycle::Running,
                Command::Stop { .. } => Lifecycle::Stopped,
                _ => Lifecycle::Deleted,
            };
            Request::Mutate {
                idempotency_key: Uuid::new_v4(),
                mutation: Mutation::State { vm_id: vm, desired },
            }
        }
        Command::Resize {
            vm,
            memory,
            cores,
            disk,
        } => {
            let snapshot = client.snapshot().await?;
            let current = snapshot
                .vms
                .iter()
                .find(|v| v.id == vm)
                .context("VM not found")?;
            Request::Mutate {
                idempotency_key: Uuid::new_v4(),
                mutation: Mutation::Resize {
                    vm_id: vm,
                    memory_gib: memory.unwrap_or(current.spec.memory_gib),
                    cores: cores.unwrap_or(current.spec.cores),
                    disk_gib: disk.unwrap_or(current.spec.disk_gib),
                },
            }
        }
        Command::Gpu { vm, pci, detach } => {
            anyhow::ensure!(pci.is_some() || detach, "Use --pci ADDRESS or --detach");
            Request::Mutate {
                idempotency_key: Uuid::new_v4(),
                mutation: Mutation::Gpu { vm_id: vm, pci },
            }
        }
        Command::Wait { operation, seconds } => {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
            loop {
                let snapshot = client.snapshot().await?;
                if let Some(op) = snapshot.operations.iter().find(|o| o.id == operation) {
                    if op.phase == "succeeded" {
                        println!("{}", serde_json::to_string_pretty(op)?);
                        return Ok(());
                    }
                    anyhow::ensure!(
                        op.phase != "needs_review" && op.phase != "failed",
                        "{}",
                        op.error.as_deref().unwrap_or("Operation requires review")
                    );
                }
                anyhow::ensure!(
                    tokio::time::Instant::now() < deadline,
                    "Operation remains pending; inspect status before retrying with the same request ID"
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        Command::SshProxy { vm } => {
            // Tokio's stdin uses a blocking read that cannot be cancelled.
            // A one-shot ProxyCommand must exit after a remote close rather
            // than wait for runtime shutdown while OpenSSH keeps stdin open.
            let code = match client.ssh_stdio(vm).await {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("SSH tunnel: {error}");
                    1
                }
            };
            std::process::exit(code);
        }
        Command::SshConfig { vm, install } => {
            let snapshot = client.snapshot().await?;
            let vm = snapshot
                .vms
                .iter()
                .find(|v| v.id == vm)
                .context("VM not found")?;
            let (path, alias) =
                homecloud_client::export_ssh(&profile, vm, &std::env::current_exe()?, install)?;
            println!(
                "{}",
                serde_json::json!({"config":path,"alias":alias,"installed":install})
            );
            return Ok(());
        }
        Command::Enroll { .. } => unreachable!(),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&client.rpc(request).await?)?
    );
    Ok(())
}
