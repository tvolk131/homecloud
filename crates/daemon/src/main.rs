use anyhow::Result;
use clap::{Parser, Subcommand};
use homecloud_client::{load_secret, private_write};
use homecloud_daemon::{Config, Service};
use homecloud_host::{Backend, ProcessBackend, Simulator};
use homecloud_store::Store;
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
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve,
    #[command(hide = true)]
    CollectStorage,
    #[command(hide = true)]
    CheckMetrics,
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
            args.simulate,
            "A real host requires an explicit --config file"
        );
        Config::default()
    };
    if matches!(args.command, Command::CheckMetrics) {
        let metrics = config
            .metrics
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("VM metrics are not configured"))?;
        let samples = homecloud_host::metrics::MetricsClient::new(metrics)?
            .fetch()
            .await?;
        println!(
            "Authenticated Incus metrics connection verified ({} managed VM samples)",
            samples.len()
        );
        return Ok(());
    }
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
    let store = Store::open(args.state_dir.join("state.db"), config.limits)?;
    match args.command {
        Command::CollectStorage | Command::CheckMetrics => unreachable!(),
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
            let backend: Arc<dyn Backend> = if args.simulate {
                Arc::new(Simulator::new(args.state_dir.join("observed.json")))
            } else {
                Arc::new(ProcessBackend {
                    script: config.host_script,
                    sudo: config.sudo,
                })
            };
            let service = Service::new(store, backend)?;
            let metrics_service = service.clone();
            let metrics_config = if args.simulate {
                None
            } else {
                config.metrics.clone()
            };
            if metrics_config.is_some() {
                service.enable_vm_metrics().await;
            }
            let metrics = tokio::spawn(async move {
                let Some(config) = metrics_config else { return };
                let mut client = None;
                loop {
                    if client.is_none() {
                        match homecloud_host::metrics::MetricsClient::new(&config) {
                            Ok(ready) => client = Some(ready),
                            Err(error) => {
                                tracing::warn!(%error, "VM metrics client unavailable");
                                let _ = metrics_service.vm_metrics_unavailable().await;
                            }
                        }
                    }
                    if let Some(ready) = client.as_ref()
                        && let Err(error) = metrics_service.collect_vm_metrics_once(ready).await
                    {
                        tracing::warn!(%error, "VM metrics unavailable");
                        client = None;
                    }
                    tokio::time::sleep(Duration::from_secs(15)).await;
                }
            });
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
            metrics.abort();
            endpoint.close().await;
        }
    }
    Ok(())
}
