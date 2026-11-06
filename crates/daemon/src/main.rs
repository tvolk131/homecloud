use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use homecloud_client::{load_secret, private_write};
use homecloud_daemon::{Config, Service};
use homecloud_host::{Backend, ProcessBackend, Simulator};
use homecloud_store::Store;
use homecloud_virtualization::{
    ProxmoxConnection, SimulatedVirtualization, VirtualizationBackend, connect_proxmox,
};
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Parser)]
#[command(about = "Homecloud single-host control plane")]
struct Args {
    #[arg(long, default_value = "/var/lib/homecloud/control")]
    state_dir: PathBuf,
    #[arg(long)]
    config: Option<PathBuf>,
    /// Explicit development backend; never starts real guest workloads.
    #[arg(long)]
    simulate: bool,
    #[arg(long, value_enum, conflicts_with_all = ["simulate", "config"])]
    virtualization: Option<VirtualizationMode>,
    #[arg(
        long,
        requires = "virtualization",
        conflicts_with = "virtualization_fixture"
    )]
    proxmox_config: Option<PathBuf>,
    #[arg(long, requires = "virtualization")]
    virtualization_fixture: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Clone, Copy, ValueEnum)]
enum VirtualizationMode {
    Simulated,
    Proxmox,
}
#[derive(Subcommand)]
enum Command {
    Serve,
    #[command(hide = true)]
    CollectStorage,
    BootstrapAdmin {
        #[arg(long, default_value = "Administrator")]
        name: String,
    },
    RecoverAdmin {
        user_id: uuid::Uuid,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    anyhow::ensure!(
        args.proxmox_config.is_none()
            || matches!(args.virtualization, Some(VirtualizationMode::Proxmox)),
        "--proxmox-config requires --virtualization proxmox"
    );
    anyhow::ensure!(
        args.virtualization_fixture.is_none()
            || matches!(args.virtualization, Some(VirtualizationMode::Simulated)),
        "--virtualization-fixture requires --virtualization simulated"
    );
    if matches!(args.command, Command::CollectStorage) {
        println!(
            "{}",
            serde_json::to_string(&homecloud_host::storage::collect().await?)?
        );
        return Ok(());
    }
    let config = if let Some(path) = args.config {
        serde_json::from_slice::<Config>(&std::fs::read(path)?)?
    } else {
        anyhow::ensure!(
            args.simulate || args.virtualization.is_some(),
            "A real host requires an explicit --config file"
        );
        Config::default()
    };
    if !args.state_dir.exists() {
        std::fs::create_dir_all(&args.state_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&args.state_dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            std::fs::metadata(&args.state_dir)?.permissions().mode() & 0o077 == 0,
            "State directory must be accessible only by its owner"
        );
    }
    let database = if args.virtualization.is_some() {
        anyhow::ensure!(
            !args.state_dir.join("state.db").exists(),
            "Virtualization mode requires a separate state directory from the legacy controller"
        );
        "virtualization.db"
    } else {
        anyhow::ensure!(
            !args.state_dir.join("virtualization.db").exists(),
            "This state directory belongs to virtualization mode"
        );
        "state.db"
    };
    let store = Store::open(args.state_dir.join(database), config.limits)?;
    match args.command {
        Command::CollectStorage => unreachable!(),
        Command::BootstrapAdmin { name } => {
            let (user, token) = store.bootstrap(&name)?;
            println!(
                "{}",
                serde_json::json!({"user_id":user.id,"token":token,"expires_in_seconds":1200})
            );
        }
        Command::RecoverAdmin { user_id } => {
            let token = store.recover_admin(user_id)?;
            println!(
                "{}",
                serde_json::json!({"user_id":user_id,"token":token,"expires_in_seconds":1200})
            );
        }
        Command::Serve => {
            if let Some(mode) = args.virtualization {
                let backend: Arc<dyn VirtualizationBackend> = match mode {
                    VirtualizationMode::Simulated => {
                        Arc::new(if let Some(path) = args.virtualization_fixture {
                            let bytes = std::fs::read(path)?;
                            anyhow::ensure!(
                                bytes.len() <= homecloud_core::MAX_FRAME,
                                "Virtualization fixture is too large"
                            );
                            SimulatedVirtualization::new(serde_json::from_slice(&bytes)?)?
                        } else {
                            SimulatedVirtualization::demo()
                        })
                    }
                    VirtualizationMode::Proxmox => {
                        let path = args.proxmox_config.ok_or_else(|| {
                            anyhow::anyhow!("--virtualization proxmox requires --proxmox-config")
                        })?;
                        let config: ProxmoxConnection =
                            serde_json::from_slice(&std::fs::read(path)?)?;
                        connect_proxmox(config)?
                    }
                };
                let service = Service::new_virtualization(store, backend)?;
                return serve_virtualization(service, &args.state_dir).await;
            }
            let backend: Arc<dyn Backend> = if args.simulate {
                Arc::new(Simulator::new(args.state_dir.join("observed.json")))
            } else {
                Arc::new(ProcessBackend {
                    script: config.host_script,
                    sudo: config.sudo,
                })
            };
            let service = Service::new(store, backend)?;
            let storage_worker = service.clone();
            let storage = tokio::spawn(async move {
                loop {
                    if let Err(error) = storage_worker.collect_storage_once().await {
                        tracing::warn!(%error, "Storage observation unavailable");
                    }
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });
            let secret = load_secret(&args.state_dir.join("endpoint.secret"))?;
            let endpoint = homecloud_daemon::endpoint(secret).await?;
            let _ = tokio::time::timeout(Duration::from_secs(15), endpoint.online()).await;
            private_write(
                &args.state_dir.join("server.json"),
                &serde_json::to_vec_pretty(&endpoint.addr())?,
            )?;
            tracing::info!(host=%endpoint.id(),"Homecloud listening; public connection ticket is server.json");
            let app_worker = service.clone();
            let applications = tokio::spawn(async move {
                loop {
                    if let Err(error) = app_worker.reconcile_applications_once().await {
                        tracing::warn!(%error, "Application observation pending");
                    }
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            });
            let worker = service.clone();
            let reconcile = tokio::spawn(async move {
                loop {
                    if let Err(error) = worker.reconcile_once().await {
                        tracing::warn!(%error,"Host reconciliation pending");
                    }
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            });
            tokio::select! {result=homecloud_daemon::serve(service,endpoint.clone())=>result?,_=tokio::signal::ctrl_c()=>{},}
            reconcile.abort();
            applications.abort();
            storage.abort();
            endpoint.close().await;
        }
    }
    Ok(())
}

async fn serve_virtualization(service: Arc<Service>, state_dir: &std::path::Path) -> Result<()> {
    let secret = load_secret(&state_dir.join("endpoint.secret"))?;
    let endpoint = homecloud_daemon::endpoint(secret).await?;
    let _ = tokio::time::timeout(Duration::from_secs(15), endpoint.online()).await;
    private_write(
        &state_dir.join("server.json"),
        &serde_json::to_vec_pretty(&endpoint.addr())?,
    )?;
    tracing::info!(host=%endpoint.id(), "Owner-only virtualization cockpit listening; legacy workers are disabled");
    let result = tokio::select! {
        result = homecloud_daemon::serve(service, endpoint.clone()) => result,
        result = tokio::signal::ctrl_c() => result.map_err(Into::into),
    };
    endpoint.close().await;
    result
}
