use crate::{Backend, HostRequest, HostResponse};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use homecloud_core::MAX_FRAME;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Stdio, sync::Arc};
use tokio::{io::AsyncWriteExt, process::Command};

/// An operator-configured command, never supplied by a network request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostCommand {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

pub struct ProcessBackend {
    pub command: HostCommand,
    pub sudo: bool,
}

#[async_trait]
impl Backend for ProcessBackend {
    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        let mut command = if self.sudo {
            let mut command = Command::new("sudo");
            command.args(["-n", "--"]).arg(&self.command.program);
            command
        } else {
            Command::new(&self.command.program)
        };
        command
            .args(&self.command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .context("Starting privileged host adapter")?;
        child
            .stdin
            .take()
            .context("Host adapter stdin")?
            .write_all(&serde_json::to_vec(&request)?)
            .await?;
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(240),
            child.wait_with_output(),
        )
        .await
        .context("Host adapter timed out; allocation remains held")??;
        ensure!(
            output.stdout.len() <= 4 * MAX_FRAME,
            "Host response too large"
        );
        ensure!(
            output.status.success(),
            "Host adapter: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: HostResponse =
            serde_json::from_slice(&output.stdout).context("Invalid host adapter response")?;
        if let HostResponse::Error(message) = response {
            anyhow::bail!("{message}");
        }
        Ok(response)
    }
}

/// Migrate read-only calls without wrapping or changing the mutation process.
pub struct ObservingBackend {
    pub primary: Arc<dyn Backend>,
    pub observer: Arc<dyn Backend>,
}

#[async_trait]
impl Backend for ObservingBackend {
    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        if matches!(
            request,
            HostRequest::Inventory | HostRequest::Observe { .. }
        ) {
            // A failed native observation stays failed; do not mask it by retrying
            // through a different implementation.
            self.observer.request(request).await
        } else {
            self.primary.request(request).await
        }
    }
}
