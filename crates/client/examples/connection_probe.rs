//! Read-only live diagnostic: compare endpoint setup, reused-client RPCs and shutdown.
//! Output contains timings and outcomes, never snapshot bodies or profile credentials.
use anyhow::{Context, Result, ensure};
use homecloud_client::{Client, ConnectionOptions, Profile};
use serde_json::json;
use std::{path::PathBuf, time::Instant};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(
        args.next()
            .context("Usage: connection_probe PROFILE [automatic|relay-only]")?,
    );
    let mode = args
        .next()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "automatic".into());
    ensure!(
        matches!(mode.as_str(), "automatic" | "relay-only"),
        "Unknown connection mode"
    );
    ensure!(args.next().is_none(), "Unexpected argument");
    let started = Instant::now();
    let client = Client::with_options(
        Profile::load(&path)?,
        ConnectionOptions {
            relay_only: mode == "relay-only",
        },
    )
    .await?;
    println!(
        "{}",
        json!({"phase":"endpoint_ready","mode":mode,"seconds":started.elapsed().as_secs_f64()})
    );
    let mut outcome = Ok(());
    for request in 1..=3 {
        let started = Instant::now();
        let response = client.snapshot().await;
        println!(
            "{}",
            json!({"phase":"snapshot","mode":mode,"request":request,"passed":response.is_ok(),"seconds":started.elapsed().as_secs_f64()})
        );
        if let Err(error) = response {
            outcome = Err(error);
            break;
        }
    }
    let started = Instant::now();
    client.close().await;
    println!(
        "{}",
        json!({"phase":"endpoint_closed","mode":mode,"seconds":started.elapsed().as_secs_f64()})
    );
    outcome
}
