use super::*;

impl Service {
    pub(super) async fn shutdown_admission(&self) -> Result<tokio::sync::RwLockWriteGuard<'_, ()>> {
        tokio::time::timeout(Duration::from_secs(2),self.shutdown_gate.write()).await
            .map_err(|_| anyhow::anyhow!("The host is busy with another operation. Refresh and retry shutdown when it finishes."))
    }

    pub(super) async fn ensure_host_not_shutting_down(&self) -> Result<()> {
        ensure!(
            self.db(|s| s.host_shutdown())
                .await?
                .is_none_or(|op| !op.phase.blocks_work()),
            "Host shutdown is in progress; review it on the Hosts page"
        );
        Ok(())
    }
    pub(super) async fn shutdown_boot(&self) -> Result<String> {
        match self.backend.request(HostRequest::ShutdownStatus).await? {
            HostResponse::ShutdownBoot(id) => {
                uuid::Uuid::parse_str(&id)?;
                Ok(id)
            }
            _ => anyhow::bail!("This host does not support shutdown"),
        }
    }
    pub(super) async fn recover_shutdown(
        &self,
        peer: String,
        id: uuid::Uuid,
        cancel: bool,
    ) -> Result<Response> {
        let _gate = self.shutdown_admission().await?;
        let boot = self.shutdown_boot().await?;
        let p = peer.clone();
        let reviewed_boot = boot.clone();
        let op = self
            .db(move |s| {
                s.sync_shutdown_boot(&reviewed_boot)?;
                s.review_host_shutdown(&p, id, cancel)
            })
            .await?;
        if op.phase != HostShutdownPhase::NeedsReview {
            return Ok(Response::HostShutdownOperation(op));
        }
        let fence = if cancel {
            HostRequest::CancelShutdown {
                operation_id: id,
                boot_id: boot.clone(),
            }
        } else {
            HostRequest::CheckShutdownFence {
                operation_id: id,
                boot_id: boot.clone(),
            }
        };
        ensure!(
            matches!(self.backend.request(fence).await?, HostResponse::Ok),
            "The host did not confirm the shutdown recovery fence"
        );
        self.db(move |s| {
            s.sync_shutdown_boot(&boot)?;
            s.recover_host_shutdown(&peer, id, cancel)
        })
        .await
        .map(Response::HostShutdownOperation)
    }
    /// One bounded guest stop per pass keeps snapshots available during a drain.
    pub(super) async fn reconcile_host_shutdown(&self) -> Result<bool> {
        let Some(mut op) = self
            .db(|s| s.host_shutdown())
            .await?
            .filter(|op| op.phase.blocks_work())
        else {
            return Ok(false);
        };
        let boot = match self.shutdown_boot().await {
            Ok(boot) => boot,
            Err(error) => {
                if matches!(
                    op.phase,
                    HostShutdownPhase::Draining | HostShutdownPhase::PoweroffPending
                ) {
                    op.phase = HostShutdownPhase::NeedsReview;
                    op.error = Some(format!(
                        "Host shutdown verification is unavailable: {error}"
                    ));
                    self.db(move |s| s.update_host_shutdown(op)).await?;
                }
                return Ok(true);
            }
        };
        let observed_boot = boot.clone();
        self.db(move |s| s.sync_shutdown_boot(&observed_boot))
            .await?;
        if op.boot_id != boot {
            return Ok(false);
        }
        let result = match op.phase {
            HostShutdownPhase::Draining => {
                if let Some(id) = op.remaining.first().copied() {
                    let vm = self.db(move |s| s.vm(id)).await?;
                    match self
                        .backend
                        .request(HostRequest::DrainGuest {
                            operation_id: op.id,
                            vm: vm.clone(),
                            boot_id: boot,
                        })
                        .await
                    {
                        Ok(HostResponse::Ok) => {
                            self.db(move |s| s.observe(id, "stopped".into(), None))
                                .await?;
                            op.remaining.remove(0);
                            Ok(())
                        }
                        Ok(_) => Err(anyhow::anyhow!("Unexpected guest shutdown response")),
                        Err(e) => Err(anyhow::anyhow!("Could not shut down {}: {e}", vm.spec.name)),
                    }
                } else {
                    op.phase = HostShutdownPhase::PoweroffPending;
                    Ok(())
                }
            }
            HostShutdownPhase::PoweroffPending => {
                let vms: Vec<Vm> = self
                    .db(|s| s.vms())
                    .await?
                    .into_iter()
                    .filter(|v| op.vm_ids.contains(&v.id))
                    .collect();
                if let Err(error) = async {
                    ensure!(
                        matches!(
                            self.backend
                                .request(HostRequest::CheckShutdown {
                                    boot_id: boot.clone(),
                                    vms: vms.clone()
                                })
                                .await?,
                            HostResponse::Ok
                        ),
                        "Guest shutdown verification was unavailable"
                    );
                    Ok::<_, anyhow::Error>(())
                }
                .await
                {
                    op.phase = HostShutdownPhase::NeedsReview;
                    op.error = Some(error.to_string());
                    self.db(move |s| s.update_host_shutdown(op)).await?;
                    return Ok(true);
                }
                let previously_attempted = op.poweroff_attempted;
                op.phase = HostShutdownPhase::PoweroffRequested;
                op.poweroff_attempted = true;
                op.poweroff_requested_at = Some(homecloud_store::now());
                op = self.db(move |s| s.update_host_shutdown(op)).await?;
                match self
                    .backend
                    .request(HostRequest::PowerOff {
                        operation_id: op.id,
                        boot_id: boot.clone(),
                        vms,
                    })
                    .await
                {
                    Ok(HostResponse::PowerOffQueued {
                        operation_id,
                        boot_id,
                    }) if operation_id == op.id && boot_id == boot => {
                        op.poweroff_requested_at = Some(homecloud_store::now());
                        Ok(())
                    }
                    Ok(HostResponse::PowerOffRejected {
                        operation_id,
                        boot_id,
                        message,
                    }) if operation_id == op.id && boot_id == boot => {
                        // Never erase an earlier uncertain attempt based on a
                        // later request that was rejected before its OS call.
                        op.poweroff_attempted = previously_attempted;
                        if !previously_attempted {
                            op.poweroff_requested_at = None;
                        }
                        Err(anyhow::anyhow!(
                            "Power-off was refused before the OS command: {message}"
                        ))
                    }
                    Ok(_) => Err(anyhow::anyhow!("Power-off acknowledgement was unavailable")),
                    Err(e) => Err(anyhow::anyhow!(
                        "Power-off could not be confirmed: {e}. It may already be queued; cancellation is unavailable. Review and retry if the host remains reachable."
                    )),
                }
            }
            HostShutdownPhase::PoweroffRequested
                if op
                    .poweroff_requested_at
                    .is_none_or(|at| homecloud_store::now().saturating_sub(at) >= 60) =>
            {
                Err(anyhow::anyhow!(
                    "Power-off was requested, but the host is still reachable after one minute. The request may still be queued. Review the host and explicitly retry; cancellation cannot keep it on."
                ))
            }
            _ => return Ok(true),
        };
        if let Err(error) = result {
            op.phase = HostShutdownPhase::NeedsReview;
            op.error = Some(error.to_string());
        }
        self.db(move |s| s.update_host_shutdown(op)).await?;
        Ok(true)
    }
}
