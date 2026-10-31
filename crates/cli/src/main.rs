use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use homecloud_client::{Client, ConnectionOptions, Profile, default_profile};
use homecloud_core::*;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[derive(Parser)]
#[command(about = "Homecloud management and OpenSSH transport")]
struct Args {
    #[arg(long, global = true)]
    profile: Option<PathBuf>,
    /// Use relay transports only to diagnose unreliable UDP/network paths.
    #[arg(long, global = true)]
    relay_only: bool,
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
    /// Adopt a reviewed physical RAM envelope; preserves all grants and claims.
    MemoryCancel {
        operation: Uuid,
    },
    MemoryAdopt {
        #[arg(long)]
        memory: u64,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        request_id: Option<Uuid>,
    },
    /// Set total system reserves (RAM/disk GiB, physical CPU cores).
    ReserveSet {
        /// Use the reviewed Homecloud ARC ceiling within system reserves.
        #[arg(long, conflicts_with = "arc_limit")]
        arc_auto: bool,
        /// Custom ARC maximum in GiB, bounded by the system reserve cache allowance.
        #[arg(long)]
        arc_limit: Option<u64>,
        #[arg(long)]
        memory: u64,
        #[arg(long)]
        cores: u64,
        #[arg(long)]
        disk: u64,
        #[arg(long)]
        expected_revision: Option<u64>,
        #[arg(long)]
        request_id: Option<Uuid>,
    },
    ReserveRetry {
        operation: Uuid,
    },
    ReserveDiscard {
        operation: Uuid,
    },
    /// Install the supported T3 application on an existing VM.
    AppInstall {
        vm: Uuid,
        #[arg(long)]
        homecloud_host: bool,
    },
    /// Start/resume T3 account authorization; inspect progress in Homecloud.
    AppSetup {
        application: Uuid,
        /// Sign into Codex instead of connecting the T3 account.
        #[arg(long, group = "target")]
        codex: bool,
        /// Sign into Claude instead of connecting the T3 account.
        #[arg(long, group = "target")]
        claude: bool,
        /// Sign Git and the GitHub CLI in this VM into GitHub.
        #[arg(long, group = "target")]
        github: bool,
        /// Remove saved GitHub CLI accounts from this VM. GitHub keeps the token valid.
        #[arg(long, group = "target")]
        github_sign_out: bool,
    },
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
        t3: bool,
        /// Install a separate disposable Incus/ZFS/Homecloud host (administrator only).
        #[arg(long, conflicts_with = "t3")]
        homecloud_host: bool,
        #[arg(long)]
        request_id: Option<Uuid>,
    },
    /// Read bounded setup diagnostics and guest logs for an owned VM.
    Diagnostics {
        vm: Uuid,
    },
    Start {
        vm: Uuid,
        #[arg(long)]
        without_gpu: bool,
    },
    Stop {
        vm: Uuid,
        /// Immediately power off the VM; unsaved work and pending writes may be lost.
        #[arg(long)]
        force: bool,
    },
    Delete {
        vm: Uuid,
    },
    /// Edit identity/resources together; running resource changes require --restart.
    Edit {
        vm: Uuid,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        memory: Option<u64>,
        #[arg(long)]
        cores: Option<u64>,
        #[arg(long)]
        disk: Option<u64>,
        /// Repeat --gpu for multiple cards (or use a comma-separated list).
        #[arg(long, conflicts_with = "no_gpu", value_delimiter = ',')]
        gpu: Vec<String>,
        #[arg(long)]
        no_gpu: bool,
        /// Explicitly consent to restarting a running VM for resource changes.
        #[arg(long)]
        restart: bool,
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
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let options = ConnectionOptions {
        relay_only: args.relay_only,
    };
    let profile = args.profile.unwrap_or_else(default_profile);
    if let Command::Enroll { ticket } = args.command {
        let server = serde_json::from_slice(&std::fs::read(ticket)?)?;
        eprint!("Invitation code: ");
        let mut token = String::new();
        std::io::stdin().read_line(&mut token)?;
        let (client, user) =
            Client::enroll_with_options(&profile, server, token.trim().into(), options).await?;
        println!("{}", serde_json::to_string_pretty(&user)?);
        client.close().await;
        return Ok(());
    }
    let client = Client::with_options(Profile::load(&profile)?, options).await?;
    let outcome = async {
    let request = match args.command {
        Command::Status => Request::Snapshot,
        Command::MemoryCancel { operation } => Request::CancelMemoryCapacity { operation_id:operation },
        Command::MemoryAdopt { memory, expected_revision, request_id } => Request::AdoptMemoryCapacity {
            target_bytes: memory.checked_mul(GIB).context("Memory overflow")?, expected_revision,
            idempotency_key: request_id.unwrap_or_else(Uuid::new_v4),
        },
        Command::ReserveSet {memory,cores,disk,expected_revision,request_id,arc_auto,arc_limit} => {
            let revision=if let Some(revision)=expected_revision {revision} else {
                client.snapshot().await?.host_reserves.context("Host reserve editing is unavailable")?.active.revision
            };
            Request::SetHostReserves {reserves:resources(memory,cores,disk)?,arc:if arc_auto {Some(ArcMode::Automatic)} else {arc_limit.map(|g|g.checked_mul(homecloud_core::GIB).map(|max_bytes|ArcMode::Custom{max_bytes}).context("ARC limit overflow")).transpose()?},expected_revision:revision,idempotency_key:request_id.unwrap_or_else(Uuid::new_v4)}
        },
        Command::ReserveRetry {operation}=>Request::RetryHostReserves {operation_id:operation},
        Command::ReserveDiscard {operation}=>Request::DiscardHostReserves {operation_id:operation},
        Command::AppInstall { vm, homecloud_host } => Request::EnsureApplication { vm_id: vm, application_kind: if homecloud_host { ApplicationKind::HomecloudHost } else { ApplicationKind::T3 }, idempotency_key: Uuid::new_v4() },
        Command::AppSetup {
            application,
            codex,
            claude,
            github,
            github_sign_out,
        } => Request::StartApplicationSetup {
            application_id: application,
            setup_action: if codex {
                ApplicationSetupAction::SignInCodex
            } else if claude {
                ApplicationSetupAction::SignInClaude
            } else if github {
                ApplicationSetupAction::SignInGitHub
            } else if github_sign_out {
                ApplicationSetupAction::SignOutGitHub
            } else {
                ApplicationSetupAction::ConnectT3
            },
            idempotency_key: Uuid::new_v4(),
        },
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
            t3,
            homecloud_host,
            request_id,
        } => Request::Mutate {
            idempotency_key: request_id.unwrap_or_else(Uuid::new_v4),
            mutation: Mutation::Create {
                spec: VmSpec {
                    applications: if homecloud_host { vec![ApplicationKind::HomecloudHost] } else if t3 { vec![ApplicationKind::T3] } else { vec![] },
                    name,
                    memory_gib: memory,
                    cores,
                    disk_gib: disk,
                    ssh_public_key: std::fs::read_to_string(ssh_key)?.trim().into(),
                },
            },
        },
        Command::Diagnostics { vm } => Request::VmDiagnostics { vm_id: vm },
        Command::Start { vm, without_gpu } => Request::Mutate {
            idempotency_key: Uuid::new_v4(),
            mutation: Mutation::Start {
                vm_id: vm,
                without_gpu,
            },
        },
        Command::Stop { vm, force } => Request::Mutate {
            idempotency_key: Uuid::new_v4(),
            mutation: if force { Mutation::ForceStop { vm_id: vm } }
                else { Mutation::State { vm_id: vm, desired: Lifecycle::Stopped } },
        },
        Command::Delete { vm } => Request::Mutate {
            idempotency_key: Uuid::new_v4(),
            mutation: Mutation::State { vm_id: vm, desired: Lifecycle::Deleted },
        },
        Command::Edit { vm,name,memory,cores,disk,gpu,no_gpu,restart } => {
            let snapshot=client.snapshot().await?;
            let current=snapshot.vms.iter().find(|v|v.id==vm).context("VM not found")?;
            let changes=WorkspaceEdit { name:name.unwrap_or_else(||current.spec.name.clone()),
                memory_gib:memory.unwrap_or(current.spec.memory_gib),cores:cores.unwrap_or(current.spec.cores),disk_gib:disk.unwrap_or(current.spec.disk_gib),
                gpu:if no_gpu || !gpu.is_empty() { GpuEdit::SetMany { pcis:gpu } } else { GpuEdit::Keep } };
            anyhow::ensure!(!changes.requires_restart(current) || restart,"Resource changes restart this running VM. Save your work, then use --restart to confirm.");
            anyhow::ensure!(changes.changed(current),"No changes requested");
            Request::Mutate {idempotency_key:Uuid::new_v4(),mutation:Mutation::Edit {vm_id:vm,changes,restart_confirmed:restart}}
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
            client.close().await;
            std::process::exit(code);
        }
        Command::SshConfig { vm, install } => {
            let snapshot = client.snapshot().await?;
            let vm = snapshot
                .vms
                .iter()
                .find(|v| v.id == vm)
                .context("VM not found")?;
            let (path, alias) = homecloud_client::export_ssh_with_options(
                &profile,
                vm,
                &std::env::current_exe()?,
                install,
                options,
            )?;
            println!(
                "{}",
                serde_json::json!({"config":path,"alias":alias,"installed":install})
            );
            return Ok(());
        }
        Command::Enroll { .. } => unreachable!(),
    };
    let mut response = client.rpc(request).await?;
    // Normal status output is safe to redirect into proof logs.
    if let Response::Snapshot(snapshot) = &mut response {
        for app in &mut snapshot.applications { app.observation.action = None; }
    }
    if let Response::GpuUnavailable(conflict) = &response {
        anyhow::bail!(
            "{conflict}. Try again later or use: homecloud start {} --without-gpu",
            conflict.vm_id
        );
    }
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
    }.await;
    client.close().await;
    outcome
}
