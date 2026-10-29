use super::*;
use uuid::Uuid;

impl Service {
    pub(super) async fn apply_memory_capacity(&self) -> Result<()> {
        let Some(op) = self.db(|s| s.pending_memory_capacity()).await? else {
            return Ok(());
        };
        let id = op.id;
        match self
            .backend
            .request(HostRequest::AdoptMemoryCapacity { operation: op })
            .await?
        {
            HostResponse::MemoryCapacity(proof) => {
                self.db(move |s| s.finish_memory_capacity(id, &proof)).await
            }
            _ => anyhow::bail!("Host did not attest memory capacity adoption"),
        }
    }
    pub(super) async fn reserve_policy(&self) -> Result<Option<HostReservePolicy>> {
        let required = self.db(|s| s.requires_host_reserve_policy()).await?;
        match self.backend.request(HostRequest::ReserveStatus).await {
            Ok(HostResponse::ReservePolicy(Some(policy))) => {
                let proof = policy.clone();
                self.db(move |s| {
                    s.require_host_reserve_policy()?;
                    s.verify_host_reserve_policy(&proof)
                })
                .await?;
                Ok(Some(policy))
            }
            Ok(HostResponse::ReservePolicy(None)) if !required => Ok(None),
            Err(error) => {
                Err(error.context("Cannot verify the host reserve policy; allocation is paused"))
            }
            _ => anyhow::bail!("Host reserve policy is unavailable; allocation is paused"),
        }
    }
    pub(super) async fn check_reserve_admission(&self) -> Result<()> {
        if let Some(policy) = self.reserve_policy().await? {
            ensure!(
                !policy.applying,
                "Host reserve enforcement is in progress; retry shortly"
            );
            ensure!(
                policy.memory.as_ref().is_none_or(|m| m.arc_ready),
                "Repair ARC enforcement before allocating resources"
            );
        }
        Ok(())
    }
    pub(super) async fn reconcile_host_reserves(&self) -> Result<()> {
        let _admission = self.reserve_admission.lock().await;
        let policy = match self.reserve_policy().await {
            Ok(policy) => policy,
            Err(error) => {
                if let Some(op) = self.db(|s| s.host_reserve_operation()).await? {
                    let text = error.to_string();
                    self.db(move |s| s.fail_host_reserves(op.id, text)).await?;
                }
                return Err(error);
            }
        };
        if let (Some(policy), Some(op)) = (&policy, self.db(|s| s.host_reserve_operation()).await?)
            && policy.state == op.target
            && !policy.applying
            && policy.memory.as_ref().is_none_or(|m| m.arc_ready)
            && policy.operation_id == Some(op.id)
        {
            let proof = policy.clone();
            self.db(move |s| s.finish_host_reserves(op.id, &proof))
                .await?;
            return Ok(());
        }
        let Some(op) = self.db(|s| s.prepare_host_reserves()).await? else {
            ensure!(
                !policy
                    .is_some_and(|p| p.applying || p.memory.as_ref().is_some_and(|m| !m.arc_ready)),
                "Host reserve update needs recovery before workload changes"
            );
            return Ok(());
        };
        let users = self.db(|s| s.users()).await?;
        let vms = self.db(|s| s.vms()).await?;
        let id = op.id;
        let result = match self
            .backend
            .request(HostRequest::ApplyReserves {
                operation: op,
                vms,
                users,
            })
            .await
        {
            Ok(HostResponse::ReservePolicy(Some(proof))) => {
                self.db(move |s| s.finish_host_reserves(id, &proof)).await
            }
            Ok(HostResponse::Error(error)) => Err(anyhow::anyhow!(error)),
            Ok(_) => Err(anyhow::anyhow!("Host did not confirm reserve enforcement")),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            let text = error.to_string();
            self.db(move |s| {
                if text.starts_with("pending:") {
                    s.wait_host_reserves(id, text)
                } else {
                    s.fail_host_reserves(id, text)
                }
            })
            .await?;
            return Err(error);
        }
        Ok(())
    }
    pub(super) async fn discard_host_reserves(&self, peer: &str, id: Uuid) -> Result<()> {
        self.reserve_policy()
            .await?
            .ok_or_else(|| anyhow::anyhow!("Reserve backend unavailable"))?;
        let actor = peer.to_owned();
        let op = self
            .db(move |s| s.prepare_reserve_discard(&actor, id))
            .await?;
        ensure!(
            op.id == id && op.phase == ReservePhase::NeedsReview,
            "Only a reserve change needing review can be discarded"
        );
        match self
            .backend
            .request(HostRequest::DiscardReserves { operation: op })
            .await?
        {
            HostResponse::DiscardedReserves {
                operation_id,
                policy,
            } if operation_id == id => {
                let peer = peer.to_owned();
                self.db(move |s| s.discard_host_reserves(&peer, id, &policy))
                    .await
            }
            _ => anyhow::bail!("Host has not confirmed the change was discarded; retry recovery"),
        }
    }
}
