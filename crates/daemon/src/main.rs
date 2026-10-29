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
    let config = if let Some(path) = args.config {
        serde_json::from_slice::<Config>(&std::fs::read(path)?)?
    } else {
        anyhow::ensure!(
            args.simulate,
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
    let store = Store::open(args.state_dir.join("state.db"), config.limits)?;
    match args.command {
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
            endpoint.close().await;
        }
    }
    Ok(())
}
