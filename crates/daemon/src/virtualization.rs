use super::*;
use homecloud_virtualization::{PowerSubmission, PowerTaskState};

struct DisabledLegacyBackend;

#[async_trait::async_trait]
impl Backend for DisabledLegacyBackend {
    async fn request(&self, _: HostRequest) -> Result<HostResponse> {
        anyhow::bail!("The legacy host adapter is disabled in virtualization mode")
    }
}

impl Service {
    pub fn new_virtualization(
        store: Store,
        backend: Arc<dyn VirtualizationBackend>,
    ) -> Result<Arc<Self>> {
        Self::new_inner(store, Arc::new(DisabledLegacyBackend), Some(backend))
    }

    pub async fn submit_virtualization_power(
        &self,
        peer: String,
        idempotency_key: uuid::Uuid,
        guest: VirtualizationGuestIdentity,
        action: VirtualizationPowerAction,
    ) -> Result<VirtualizationOperation> {
        let backend = self
            .virtualization
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Virtualization mode is not enabled"))?;
        let actor = peer.clone();
        ensure!(
            self.db(move |store| store.authenticate(&actor)).await?.role == Role::Admin,
            "Administrator required"
        );
        guest.validate()?;
        let operation = VirtualizationOperation {
            id: idempotency_key,
            requested_by: peer,
            guest,
            action,
            phase: VirtualizationOperationPhase::Submitting,
            task_id: None,
            error: None,
            created_at: homecloud_store::now(),
            updated_at: homecloud_store::now(),
        };
        let (operation, created) = self
            .db(move |store| store.reserve_virtualization_operation(operation))
            .await?;
        if created {
            let store = self.store.clone();
            let pending = operation.clone();
            tokio::spawn(async move {
                if let Err(error) = dispatch(store, backend, pending).await {
                    tracing::error!(%error, "Power submission tracking failed; no automatic retry will occur");
                }
            });
        }
        Ok(operation)
    }

    pub async fn refresh_virtualization_operations(&self) -> Result<()> {
        let _guard = self.virtualization_refresh.lock().await;
        let backend = self
            .virtualization
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Virtualization mode is not enabled"))?;
        let operations = self.db(|store| store.virtualization_operations()).await?;
        let mut tasks = tokio::task::JoinSet::new();
        for mut operation in operations {
            if operation.phase != VirtualizationOperationPhase::Running {
                continue;
            }
            let backend = backend.clone();
            tasks.spawn(async move {
                let result = match operation.task_id.as_deref() {
                    Some(task) => backend.power_task(&operation.guest, operation.action, task).await,
                    None => Err(anyhow::anyhow!("Missing task identity; inspect Proxmox")),
                };
                operation.error = None;
                match result {
                    Ok(PowerTaskState::Running) => {}
                    Ok(PowerTaskState::Succeeded) => operation.phase = VirtualizationOperationPhase::Succeeded,
                    Ok(PowerTaskState::Failed) => {
                        operation.phase = VirtualizationOperationPhase::Failed;
                        operation.error = Some("Proxmox task failed. Inspect its task log before trying another action. Graceful shutdown never falls back to force-stop.".into());
                    }
                    Err(error) => operation.error = Some(error.to_string()),
                }
                operation.updated_at = homecloud_store::now();
                operation
            });
        }
        while let Some(result) = tasks.join_next().await {
            let operation = result?;
            self.db(move |store| store.update_virtualization_operation(&operation))
                .await?;
        }
        Ok(())
    }
}

async fn dispatch(
    store: Store,
    backend: Arc<dyn VirtualizationBackend>,
    mut operation: VirtualizationOperation,
) -> Result<()> {
    let check_store = store.clone();
    let peer = operation.requested_by.clone();
    let preflight = async {
        let actor = tokio::task::spawn_blocking(move || check_store.authenticate(&peer)).await??;
        ensure!(actor.role == Role::Admin, "Administrator required");
        let inventory = backend.inventory().await?;
        let guest = inventory
            .guests
            .iter()
            .find(|guest| {
                guest.node == operation.guest.node
                    && guest.vmid == operation.guest.vmid
                    && guest.kind == operation.guest.kind
            })
            .ok_or_else(|| {
                anyhow::anyhow!("Guest is no longer visible on this node; refresh inventory")
            })?;
        ensure!(!guest.template, "Templates cannot be powered on or off");
        let expected = if operation.action == VirtualizationPowerAction::Start {
            "stopped"
        } else {
            "running"
        };
        ensure!(
            guest.status == expected,
            "Guest status changed or is unsupported; refresh inventory before issuing a new action"
        );
        anyhow::Ok(())
    }
    .await;
    let outcome = match preflight {
        Ok(()) => {
            backend
                .submit_power(&operation.guest, operation.action)
                .await
        }
        Err(error) => PowerSubmission::Rejected(error.to_string()),
    };
    match outcome {
        PowerSubmission::Accepted(task) => {
            operation.phase = VirtualizationOperationPhase::Running;
            operation.task_id = Some(task);
        }
        PowerSubmission::Rejected(error) => {
            operation.phase = VirtualizationOperationPhase::Failed;
            operation.error = Some(error);
        }
        PowerSubmission::Unknown(error) => {
            operation.phase = VirtualizationOperationPhase::Unknown;
            operation.error = Some(error);
        }
    }
    operation.updated_at = homecloud_store::now();
    tokio::task::spawn_blocking(move || {
        for attempt in 0..3 {
            match store.update_virtualization_operation(&operation) {
                Ok(()) => return Ok(()),
                Err(error) if attempt == 2 => {
                    tracing::error!(
                        operation_id = %operation.id,
                        node = %operation.guest.node,
                        vmid = operation.guest.vmid,
                        task_id = ?operation.task_id,
                        %error,
                        "Power result could not be persisted; inspect this task before recovery"
                    );
                    return Err(error);
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(100 << attempt)),
            }
        }
        unreachable!()
    })
    .await?
}
