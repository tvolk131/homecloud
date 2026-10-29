//! Explicit live-host diagnostic: authenticate and optionally reach guest SSH
//! with every direct IP transport disabled. Uses an already enrolled device.
use anyhow::{Context, Result, ensure};
use homecloud_client::{DeviceProof, Profile, read_frame, write_frame};
use homecloud_core::{CONTROL_ALPN, Request, Response, SSH_ALPN};
use iroh::{Endpoint, SecretKey, endpoint::presets};
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let count: usize = std::env::var("HOMECLOUD_PROBE_COUNT")
        .unwrap_or_else(|_| "1".into())
        .parse()?;
    ensure!((1..=30).contains(&count), "Probe count must be 1..=30");
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(
        args.next()
            .context("Usage: relay_probe PROFILE_JSON [VM_UUID]")?,
    );
    let vm = args
        .next()
        .map(|s| s.to_string_lossy().parse())
        .transpose()?;
    ensure!(args.next().is_none(), "Unexpected argument");
    tokio::time::timeout(Duration::from_secs(45 * count as u64), async {
        let profile = Profile::load(&path)?;
        let bytes: [u8; 32] = hex::decode(profile.device_secret)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid device credential"))?;
        let device = SecretKey::from_bytes(&bytes);
        let endpoint = Endpoint::builder(presets::N0)
            .clear_ip_transports()
            .bind()
            .await?;
        for attempt in 0..count {
            let started = std::time::Instant::now();
            let connection = endpoint
                .connect(profile.server.clone(), CONTROL_ALPN)
                .await?;
            ensure!(
                connection.paths().iter().any(|p| p.is_relay()),
                "No relay path"
            );
            ensure!(
                !connection.paths().iter().any(|p| p.is_ip()),
                "Unexpected direct IP path"
            );
            let (mut send, mut recv) = connection.open_bi().await?;
            write_frame(&mut send, &DeviceProof::sign(&connection, &device)?).await?;
            write_frame(&mut send, &Request::Snapshot).await?;
            send.finish()?;
            let response: Response = read_frame(&mut recv).await?;
            let Response::Snapshot(snapshot) = response else {
                anyhow::bail!("Relay probe did not receive an authenticated snapshot");
            };
            println!(
                "Authenticated control RPC via relay {}/{}: {}; {:.2}s",
                attempt + 1,
                count,
                snapshot.inventory.hostname,
                started.elapsed().as_secs_f64()
            );
            connection.close(0u32.into(), b"relay control probe complete");
        }
        if let Some(vm_id) = vm {
            let connection = endpoint.connect(profile.server, SSH_ALPN).await?;
            ensure!(
                connection.paths().iter().any(|p| p.is_relay()),
                "No SSH relay path"
            );
            ensure!(
                !connection.paths().iter().any(|p| p.is_ip()),
                "Unexpected direct SSH path"
            );
            let (mut send, mut recv) = connection.open_bi().await?;
            write_frame(&mut send, &DeviceProof::sign(&connection, &device)?).await?;
            write_frame(&mut send, &Request::Ssh { vm_id }).await?;
            let response: Response = read_frame(&mut recv).await?;
            ensure!(
                matches!(response, Response::SshReady),
                "SSH route was not authorized"
            );
            let mut banner = [0u8; 256];
            let n = tokio::io::AsyncReadExt::read(&mut recv, &mut banner).await?;
            ensure!(
                banner[..n].starts_with(b"SSH-2.0-"),
                "Guest SSH banner missing"
            );
            println!("Authorized guest SSH byte stream via relay: {vm_id}");
            send.finish()?;
            connection.close(0u32.into(), b"relay SSH probe complete");
        }
        endpoint.close().await;
        Ok(())
    })
    .await
    .context("Relay probe timed out")?
}
