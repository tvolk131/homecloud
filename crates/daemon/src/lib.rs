#[cfg(test)]
mod action_tests;
mod apps;
#[cfg(test)]
mod memory_tests;
mod metrics;
#[cfg(test)]
mod reserve_tests;
mod reserves;
mod shutdown;
#[cfg(test)]
mod shutdown_tests;
mod storage;
#[cfg(test)]
mod storage_tests;
use anyhow::{Result, ensure};
use homecloud_client::{DeviceProof, read_frame, write_frame};
use homecloud_core::*;
use homecloud_host::{Backend, HostRequest, HostResponse};
use homecloud_store::{Limits, Store};
use iroh::{
    Endpoint, SecretKey,
    endpoint::{Connection, presets},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, RwLock, Semaphore},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub limits: Limits,
    pub host_script: PathBuf,
    #[serde(default = "default_true")]
    pub sudo: bool,
    #[serde(default)]
    pub metrics: Option<homecloud_host::metrics::MetricsConfig>,
}
fn default_true() -> bool {
    true
}
impl Default for Config {
    fn default() -> Self {
        Self {
            limits: Limits {
                capacity: Resources {
                    memory_bytes: 32 * GIB,
                    cpu_cores: 2,
                    disk_bytes: 128 * GIB,
                },
                core_groups: vec![vec![2, 8], vec![3, 9]],
                disk_overhead_gib_per_vm: 2,
            },
            host_script: "/usr/local/libexec/homecloud/host_runtime.py".into(),
            sudo: true,
            metrics: None,
        }
    }
}

pub struct Service {
    pub store: Store,
    pub backend: Arc<dyn Backend>,
    enrollment_attempts: Mutex<HashMap<String, (Instant, u8)>>,
    reconciliation: Mutex<()>,
    application_reconciliation: Mutex<()>,
    reserve_admission: Mutex<()>,
    shutdown_gate: RwLock<()>,
    application_actions: Mutex<HashMap<uuid::Uuid, BrowserAction>>,
    storage: RwLock<StorageInventory>,
    storage_collection: Mutex<()>,
    metrics: RwLock<metrics::MetricsCache>,
    _controller_lock: std::fs::File,
}
impl Service {
    pub fn new(store: Store, backend: Arc<dyn Backend>) -> Result<Arc<Self>> {
        let controller_lock = store.lock_controller()?;
        store.interrupt_host_shutdown()?;
        Ok(Arc::new(Self {
            store,
            backend,
            enrollment_attempts: Mutex::new(HashMap::new()),
            reconciliation: Mutex::new(()),
            application_reconciliation: Mutex::new(()),
            reserve_admission: Mutex::new(()),
            shutdown_gate: RwLock::new(()),
            application_actions: Mutex::new(HashMap::new()),
            storage: RwLock::new(StorageInventory::default()),
            storage_collection: Mutex::new(()),
            metrics: RwLock::new(metrics::MetricsCache::default()),
            _controller_lock: controller_lock,
        }))
    }
    async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(store)).await?
    }
    async fn enrollment_allowed(&self, peer: &str) -> bool {
        let mut attempts = self.enrollment_attempts.lock().await;
        attempts.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(60));
        if attempts.len() > 4096 {
            return false;
        }
        let entry = attempts.entry(peer.into()).or_insert((Instant::now(), 0));
        entry.1 = entry.1.saturating_add(1);
        entry.1 <= 5
    }
    pub async fn request(&self, peer: String, request: Request) -> Result<Response> {
        if let Request::Enroll { token } = request {
            // Possession of an already enrolled active device makes retrying
            // a lost enrollment response safe; it never creates another device.
            let p = peer.clone();
            if let Ok(user) = self.db(move |s| s.authenticate(&p)).await {
                return Ok(Response::Enrolled(user));
            }
            ensure!(
                self.enrollment_allowed(&peer).await,
                "Too many enrollment attempts; retry later"
            );
            return Ok(Response::Enrolled(
                self.db(move |s| s.enroll(&token, &peer)).await?,
            ));
        }
        let p = peer.clone();
        let me = self.db(move |s| s.authenticate(&p)).await?;
        let _gate = if request.changes_host_workload() {
            let gate = self.shutdown_gate.read().await;
            self.ensure_host_not_shutting_down().await?;
            Some(gate)
        } else {
            None
        };
        match request {
            Request::ShutdownHost {
                idempotency_key,
                expected_boot_id,
            } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                let p = peer.clone();
                let expected = expected_boot_id.clone();
                if let Some(op) = self
                    .db(move |s| s.review_shutdown_request(&p, idempotency_key, &expected))
                    .await?
                {
                    return Ok(Response::HostShutdownOperation(op));
                }
                let _gate = self.shutdown_admission().await?;
                let boot_id = self.shutdown_boot().await?;
                ensure!(
                    boot_id == expected_boot_id,
                    "Host restarted; refresh before shutting down"
                );
                self.db(move |s| s.request_host_shutdown(&peer, idempotency_key, boot_id))
                    .await
                    .map(Response::HostShutdownOperation)
            }
            Request::RetryHostShutdown { operation_id } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                self.recover_shutdown(peer, operation_id, false).await
            }
            Request::CancelHostShutdown { operation_id } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                self.recover_shutdown(peer, operation_id, true).await
            }
            Request::CancelMemoryCapacity { operation_id } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                let _worker = self.reconciliation.lock().await;
                let _admission = self.reserve_admission.lock().await;
                let op = self
                    .db(|s| s.pending_memory_capacity())
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("No pending memory capacity update"))?;
                ensure!(
                    op.id == operation_id,
                    "Memory operation changed; refresh before cancelling"
                );
                match self
                    .backend
                    .request(HostRequest::CancelMemoryCapacity { operation: op })
                    .await?
                {
                    HostResponse::MemoryCapacityCancelled {
                        operation_id: id,
                        status,
                    } if id == operation_id => {
                        self.db(move |s| s.cancel_memory_capacity(&peer, id, &status))
                            .await?;
                        Ok(Response::Ok)
                    }
                    _ => anyhow::bail!("Host did not confirm the unapplied update was fenced"),
                }
            }
            Request::AdoptMemoryCapacity {
                expected_revision,
                target_bytes,
                idempotency_key,
            } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                let _worker = self.reconciliation.lock().await;
                let _admission = self.reserve_admission.lock().await;
                self.reserve_policy().await?;
                self.db(move |s| {
                    s.memory_capacity_request(
                        &peer,
                        idempotency_key,
                        expected_revision,
                        target_bytes,
                    )
                })
                .await?;
                self.apply_memory_capacity().await?;
                Ok(Response::Ok)
            }
            Request::SetHostReserves {
                reserves,
                arc,
                expected_revision,
                idempotency_key,
            } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                let _admission = self.reserve_admission.lock().await;
                let policy = self
                    .reserve_policy()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("This host does not support reserve editing"))?;
                ensure!(
                    !policy.applying,
                    "Host reserve update is still being applied"
                );
                let additional = reserves
                    .checked_sub(policy.minimum_reserves)
                    .map_err(anyhow::Error::msg)?;
                policy
                    .base_pool_free_bytes
                    .checked_add(additional.disk_bytes)
                    .ok_or_else(|| anyhow::anyhow!("Pool floor overflow"))?;
                Ok(Response::HostReserveOperation(
                    self.db(move |s| {
                        s.set_host_policy(
                            &peer,
                            idempotency_key,
                            expected_revision,
                            additional,
                            arc,
                            policy.memory_policy,
                        )
                    })
                    .await?,
                ))
            }
            Request::RetryHostReserves { operation_id } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                let _admission = self.reserve_admission.lock().await;
                self.reserve_policy()
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Reserve backend unavailable"))?;
                Ok(Response::HostReserveOperation(
                    self.db(move |s| s.retry_host_reserves(&peer, operation_id))
                        .await?,
                ))
            }
            Request::DiscardHostReserves { operation_id } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                let _worker = self.reconciliation.lock().await;
                let _admission = self.reserve_admission.lock().await;
                self.discard_host_reserves(&peer, operation_id).await?;
                Ok(Response::Ok)
            }
            Request::Snapshot => {
                let users = self.db(|s| s.users()).await?;
                let p = peer.clone();
                let devices = self.db(move |s| s.devices(&p)).await?;
                let vms: Vec<Vm> = self
                    .db(|s| s.vms())
                    .await?
                    .into_iter()
                    .filter(|v| {
                        v.observed != "deleted" && (me.role == Role::Admin || v.owner == me.id)
                    })
                    .collect();
                let operations = self
                    .db(|s| s.operations())
                    .await?
                    .into_iter()
                    .filter(|o| me.role == Role::Admin || o.actor == me.id)
                    .collect();
                let mut inventory = match self.backend.request(HostRequest::Inventory).await {
                    Ok(HostResponse::Inventory(inventory)) => inventory,
                    Err(error) => HostInventory {
                        hostname: "Host inventory unavailable".into(),
                        warnings: vec![error.to_string()],
                        ..Default::default()
                    },
                    _ => HostInventory::default(),
                };
                inventory.storage = if me.role == Role::Admin {
                    Some(self.storage.read().await.clone())
                } else {
                    None
                };
                let users = users
                    .into_iter()
                    .filter(|u| me.role == Role::Admin || u.id == me.id)
                    .collect();
                let visible: Vec<_> = vms.iter().map(|v| v.id).collect();
                let actions = self.application_actions.lock().await;
                let applications: Vec<_> = self
                    .db(|s| s.applications())
                    .await?
                    .into_iter()
                    .filter(|a| visible.contains(&a.vm_id))
                    .map(|mut a| {
                        if !vms.iter().any(|v| v.id == a.vm_id && v.owner == me.id) {
                            a.observation.accounts.clear();
                        }
                        if vms.iter().any(|v| {
                            v.id == a.vm_id
                                && v.desired == Lifecycle::Running
                                && v.observed == "running"
                        }) {
                            a.observation.action = actions
                                .get(&a.id)
                                .filter(|x| {
                                    x.expires_at > homecloud_store::now()
                                        && (x.setup_action == ApplicationSetupAction::ConnectT3
                                            || vms
                                                .iter()
                                                .any(|v| v.id == a.vm_id && v.owner == me.id))
                                })
                                .cloned();
                        }
                        a
                    })
                    .collect();
                let ids: Vec<_> = applications.iter().map(|a| a.id).collect();
                let application_operations = self
                    .db(|s| s.application_operations())
                    .await?
                    .into_iter()
                    .filter(|op| ids.contains(&op.application_id))
                    .collect();
                // All users need the startup condition; only administrators get
                // aggregate allocations and reserve configuration.
                let policy = self.reserve_policy().await;
                let verified = policy.is_ok();
                let host_reserves = match policy {
                    Ok(Some(policy)) if me.role == Role::Admin => {
                        match self.db(move |s| s.host_reserve_view(&policy)).await {
                            Ok(view) => Some(view),
                            Err(error) => {
                                inventory.warnings.push(error.to_string());
                                None
                            }
                        }
                    }
                    Err(error) => {
                        if me.role == Role::Admin {
                            inventory.warnings.push(error.to_string());
                        }
                        None
                    }
                    _ => None,
                };
                let memory_peer = peer.clone();
                let memory_health = self
                    .db(move |s| s.memory_health(&memory_peer, verified))
                    .await?;
                let capacity = self.db(|s| s.capacity()).await?;
                let mut host_shutdown = self.db(|s| s.host_shutdown()).await?;
                if me.role != Role::Admin
                    && let Some(op) = &mut host_shutdown
                {
                    op.vm_ids.retain(|id| vms.iter().any(|vm| vm.id == *id));
                    op.remaining.retain(|id| vms.iter().any(|vm| vm.id == *id));
                    op.error = None;
                }
                Ok(Response::Snapshot(Box::new(Snapshot {
                    vm_metrics: self
                        .metrics
                        .read()
                        .await
                        .for_vms(&vms, homecloud_store::now()),
                    host_shutdown,
                    host_reserves,
                    memory_health,
                    disk_overhead_gib_per_vm: self.store.limits().disk_overhead_gib_per_vm,
                    applications,
                    application_operations,
                    me,
                    users,
                    devices,
                    vms,
                    operations,
                    inventory,
                    capacity,
                })))
            }
            Request::EnsureApplication {
                vm_id,
                application_kind,
                idempotency_key,
            } => Ok(Response::ApplicationOperation(
                self.db(move |s| {
                    s.ensure_application(&peer, vm_id, application_kind, idempotency_key)
                })
                .await?,
            )),
            Request::StartApplicationSetup {
                application_id,
                setup_action,
                idempotency_key,
            } => Ok(Response::ApplicationOperation(
                self.db(move |s| {
                    s.start_application_setup(&peer, application_id, idempotency_key, setup_action)
                })
                .await?,
            )),
            Request::SubmitApplicationCode {
                application_id,
                operation_id,
                code,
            } => {
                ensure!(
                    (8..=2048).contains(&code.0.len())
                        && code.0.is_ascii()
                        && !code.0.chars().any(char::is_control),
                    "Invalid authorization code"
                );
                let (application, vm, operation) = self
                    .db(move |s| s.authorize_application_code(&peer, application_id, operation_id))
                    .await?;
                let valid = self
                    .application_actions
                    .lock()
                    .await
                    .get(&application_id)
                    .is_some_and(|a| {
                        a.setup_action == ApplicationSetupAction::SignInClaude
                            && a.input_required
                            && a.expires_at > homecloud_store::now()
                    });
                ensure!(
                    valid && application.observation.operation_id == Some(operation_id),
                    "The sign-in link expired or is no longer waiting for a code"
                );
                match self
                    .backend
                    .request(HostRequest::Application {
                        vm,
                        application: Box::new(application),
                        command: ApplicationCommand::SubmitCode { operation, code },
                    })
                    .await
                {
                    Ok(HostResponse::Application(_)) => Ok(Response::Ok),
                    _ => anyhow::bail!(
                        "Could not deliver the code to this sign-in attempt. Refresh its status and try again."
                    ),
                }
            }
            Request::Invite {
                name,
                role,
                grant,
                gpu_allowed,
                expires_hours,
            } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                let _admission = self.reserve_admission.lock().await;
                self.check_reserve_admission().await?;
                let (token, user_id, expires_at) = self
                    .db(move |s| s.invite(&peer, &name, role, grant, gpu_allowed, expires_hours))
                    .await?;
                Ok(Response::Invitation {
                    token,
                    user_id,
                    expires_at,
                })
            }
            Request::InviteDevice {
                user_id,
                expires_hours,
            } => {
                let (token, expires_at) = self
                    .db(move |s| s.invite_device(&peer, user_id, expires_hours))
                    .await?;
                Ok(Response::Invitation {
                    token,
                    user_id,
                    expires_at,
                })
            }
            Request::Grant {
                user_id,
                grant,
                gpu_allowed,
            } => {
                ensure!(me.role == Role::Admin, "Administrator required");
                let _admission = self.reserve_admission.lock().await;
                self.check_reserve_admission().await?;
                self.db(move |s| s.grant(&peer, user_id, grant, gpu_allowed))
                    .await?;
                Ok(Response::Ok)
            }
            Request::RevokeDevice { peer: target } => {
                self.db(move |s| s.revoke_device(&peer, &target)).await?;
                Ok(Response::Ok)
            }
            Request::SetAccountActive { user_id, active } => {
                self.db(move |s| s.set_account_active(&peer, user_id, active))
                    .await?;
                Ok(Response::Ok)
            }
            Request::CreateVm {
                idempotency_key,
                spec,
            } => {
                let _admission = self.reserve_admission.lock().await;
                self.check_reserve_admission().await?;
                match self
                    .db(move |s| s.submit_create(&peer, idempotency_key, spec))
                    .await?
                {
                    homecloud_store::CreateOutcome::Accepted(operation) => {
                        Ok(Response::Operation(operation))
                    }
                    homecloud_store::CreateOutcome::Rejected(message) => {
                        Ok(Response::CreateRejected {
                            idempotency_key,
                            message,
                        })
                    }
                }
            }
            Request::Mutate {
                idempotency_key,
                mutation,
            } => {
                let _worker = if matches!(
                    &mutation,
                    Mutation::State {
                        desired: Lifecycle::Deleted,
                        ..
                    }
                ) {
                    Some(self.reconciliation.lock().await)
                } else {
                    None
                };
                let _admission = self.reserve_admission.lock().await;
                let releasing = mutation.stops_vm()
                    || matches!(
                        mutation,
                        Mutation::State {
                            desired: Lifecycle::Deleted,
                            ..
                        }
                    );
                if releasing {
                    let _ = self.reserve_policy().await;
                } else {
                    self.check_reserve_admission().await?;
                }
                let starting = matches!(
                    &mutation,
                    Mutation::Start { .. }
                        | Mutation::State {
                            desired: Lifecycle::Running,
                            ..
                        }
                );
                match self
                    .db(move |s| s.submit(&peer, idempotency_key, mutation))
                    .await
                {
                    Ok(operation) => Ok(Response::Operation(operation)),
                    Err(error) => {
                        if starting && let Some(conflict) = error.downcast_ref::<GpuUnavailable>() {
                            Ok(Response::GpuUnavailable(conflict.clone()))
                        } else {
                            Err(error)
                        }
                    }
                }
            }
            Request::VmDiagnostics { vm_id } => {
                let vm = self.db(move |s| s.owned_vm(&peer, vm_id)).await?;
                ensure!(vm.observed != "deleted", "VM has been deleted");
                match self
                    .backend
                    .request(HostRequest::Diagnostics { vm })
                    .await?
                {
                    HostResponse::Diagnostics(report) if report.vm_id == vm_id => {
                        Ok(Response::VmDiagnostics(report))
                    }
                    _ => anyhow::bail!("Unexpected VM diagnostics response"),
                }
            }
            Request::RetryOperation { operation_id } => {
                self.db(move |s| s.retry_operation(&peer, operation_id))
                    .await?;
                Ok(Response::Ok)
            }
            Request::Ssh { .. } => anyhow::bail!("Use the SSH transport protocol"),
            Request::Enroll { .. } => unreachable!(),
        }
    }
    pub async fn reconcile_once(&self) -> Result<()> {
        let _gate = self.shutdown_gate.read().await;
        let _worker = self.reconciliation.lock().await;
        if self.reconcile_host_shutdown().await? {
            if self
                .db(|s| s.host_shutdown())
                .await?
                .is_some_and(|op| op.phase == HostShutdownPhase::NeedsReview)
            {
                self.observe_vms_once().await?;
            }
            return Ok(());
        }
        {
            let _admission = self.reserve_admission.lock().await;
            if let Err(error) = self.apply_memory_capacity().await {
                tracing::warn!(%error,"Memory capacity adoption needs recovery");
            }
        }
        let reserve_result = self.reconcile_host_reserves().await;
        self.db(|s| s.resume_host_boot()).await?;
        let users = self.db(|s| s.users()).await?;
        // Deletion and other releasing operations must remain possible even
        // while storage headroom is low. Each apply validates its own backing.
        if let Err(error) = self
            .backend
            .request(HostRequest::Backing {
                users: users.clone(),
            })
            .await
        {
            tracing::warn!(%error,"Storage grants awaiting physical backing");
        }
        for op in self.db(|s| s.pending()).await? {
            if reserve_result.is_err()
                && !op.request.stops_vm()
                && !matches!(
                    op.request,
                    Mutation::State {
                        desired: Lifecycle::Deleted,
                        ..
                    }
                )
            {
                continue;
            }
            let id = op.id;
            let Some(context) = self.db(move |s| s.prepare_apply(id)).await? else {
                continue;
            };
            match self
                .backend
                .request(HostRequest::Apply {
                    vm: context.vm,
                    vms: context.vms,
                    users: context.users,
                    mutation: context.operation.request,
                })
                .await
            {
                Ok(HostResponse::Applied(observation)) => {
                    if let Err(error) = self
                        .db(move |s| {
                            s.complete(
                                id,
                                observation.state,
                                observation.ssh_host_key,
                                observation.compute_released,
                            )
                        })
                        .await
                    {
                        self.db(move |s| {
                            s.operation_phase(id, "needs_review", Some(error.to_string()))
                        })
                        .await?;
                    }
                }
                Ok(_) => {
                    self.db(move |s| {
                        s.operation_phase(
                            id,
                            "needs_review",
                            Some("Unexpected host response".into()),
                        )
                    })
                    .await?;
                }
                Err(error) => {
                    let message = error.to_string();
                    let phase = if message.starts_with("conflict:") || op.request.stops_vm() {
                        "needs_review"
                    } else {
                        "retrying"
                    };
                    self.db(move |s| s.operation_phase(id, phase, Some(message)))
                        .await?;
                }
            }
        }
        self.observe_vms_once().await?;
        reserve_result?;
        Ok(())
    }
    async fn observe_vms_once(&self) -> Result<()> {
        let vms = self.db(|s| s.vms()).await?;
        if let HostResponse::Observations(observed) =
            self.backend.request(HostRequest::Observe { vms }).await?
        {
            for vm in observed {
                if let Some(progress) = vm.progress {
                    self.db(move |s| s.observe_setup_progress(vm.id, progress))
                        .await?;
                }
                self.db(move |s| s.observe(vm.id, vm.state, vm.error))
                    .await?;
            }
        }
        Ok(())
    }
    async fn connection(self: Arc<Self>, conn: Connection) -> Result<()> {
        let (mut send, mut recv) =
            tokio::time::timeout(Duration::from_secs(15), conn.accept_bi()).await??;
        let proof: DeviceProof =
            tokio::time::timeout(Duration::from_secs(15), read_frame(&mut recv)).await??;
        let peer = match proof.verify(&conn) {
            Ok(device) => device,
            Err(error) => {
                conn.close(403u32.into(), b"device proof rejected");
                return Err(error);
            }
        };
        let request: Request =
            tokio::time::timeout(Duration::from_secs(15), read_frame(&mut recv)).await??;
        if conn.alpn() == SSH_ALPN {
            let Request::Ssh { vm_id } = request else {
                anyhow::bail!("Invalid SSH request");
            };
            let p = peer.clone();
            let vm = match self.db(move |s| s.owned_vm(&p, vm_id)).await {
                Ok(vm) => vm,
                Err(error) => {
                    write_frame(&mut send, &Response::error(error)).await?;
                    send.finish()?;
                    let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
                    return Ok(());
                }
            };
            if vm.desired != Lifecycle::Running || vm.observed != "running" {
                write_frame(&mut send, &Response::error("VM is not running")).await?;
                send.finish()?;
                let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
                return Ok(());
            }
            if let Err(error) = self
                .backend
                .request(HostRequest::VerifySsh { vm: vm.clone() })
                .await
            {
                write_frame(&mut send, &Response::error(error)).await?;
                send.finish()?;
                let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
                return Ok(());
            }
            // Address and port are derived exclusively from the persisted VM;
            // the client supplies no destination address, port, or guest command.
            let socket = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::TcpStream::connect((vm.address(), 22)),
            )
            .await??;
            write_frame(&mut send, &Response::SshReady).await?;
            let (mut read_tcp, mut write_tcp) = socket.into_split();
            let forwarding = async {
                tokio::try_join!(
                    async {
                        tokio::io::copy(&mut recv, &mut write_tcp).await?;
                        write_tcp.shutdown().await?;
                        anyhow::Ok(())
                    },
                    async {
                        tokio::io::copy(&mut read_tcp, &mut send).await?;
                        send.finish()?;
                        anyhow::Ok(())
                    }
                )?;
                anyhow::Ok(())
            };
            let revocation = async {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let p = peer.clone();
                    let current = self.db(move |s| s.owned_vm(&p, vm_id)).await;
                    if !matches!(current,Ok(ref v) if v.desired==Lifecycle::Running) {
                        conn.close(403u32.into(), b"access revoked");
                        break;
                    }
                }
            };
            tokio::select! {result=forwarding=>result?,_=revocation=>{},}
        } else {
            let response = match self.request(peer.clone(), request).await {
                Ok(response) => response,
                Err(error) => {
                    let _ = self.db(move |s| s.audit_failure(&peer)).await;
                    Response::error(error)
                }
            };
            write_frame(&mut send, &response).await?;
            send.finish()?;
        }
        // Give QUIC time to deliver FIN rather than dropping all stream handles.
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
        Ok(())
    }
}

pub async fn endpoint(secret: SecretKey) -> Result<Endpoint> {
    Ok(Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![CONTROL_ALPN.to_vec(), SSH_ALPN.to_vec()])
        .bind()
        .await?)
}
pub async fn serve(service: Arc<Service>, endpoint: Endpoint) -> Result<()> {
    let connections = Arc::new(Semaphore::new(64));
    while let Some(incoming) = endpoint.accept().await {
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            incoming.refuse();
            continue;
        };
        let service = service.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Ok(Ok(conn)) = tokio::time::timeout(Duration::from_secs(15), incoming).await
                && let Err(error) = service.connection(conn).await
            {
                tracing::debug!(%error,"Connection ended");
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homecloud_host::Simulator;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingBackend {
        inner: Simulator,
        active: AtomicUsize,
        maximum: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Backend for CountingBackend {
        async fn request(&self, request: HostRequest) -> Result<HostResponse> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            // Yield before the simulator takes its own lock: overlapping
            // reconciliation would be visible even if host commands serialize.
            tokio::time::sleep(Duration::from_millis(10)).await;
            let result = self.inner.request(request).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        }
    }
    #[tokio::test]
    async fn only_one_controller_and_one_reconciliation_pass_can_apply_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
        let host = Arc::new(CountingBackend {
            inner: Simulator::new(dir.path().join("observed.json")),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
        });
        let service = Service::new(store.clone(), host.clone()).unwrap();
        assert!(Service::new(store.clone(), host.clone()).is_err());
        let (first, second) = tokio::join!(service.reconcile_once(), service.reconcile_once());
        first.unwrap();
        second.unwrap();
        assert_eq!(host.maximum.load(Ordering::SeqCst), 1);
        drop(service);
        assert!(Service::new(store, host).is_ok());
    }
    #[tokio::test]
    async fn replay_after_host_effect_converges_without_duplicate_allocation() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::default();
        let path = dir.path().join("state.db");
        let store = Store::open(&path, cfg.limits.clone()).unwrap();
        let (user, token) = store.bootstrap("Admin").unwrap();
        store.enroll(&token, "device").unwrap();
        store
            .grant("device", user.id, cfg.limits.capacity, true)
            .unwrap();
        let op=store.submit("device",uuid::Uuid::new_v4(),Mutation::Create{spec:VmSpec{applications:vec![],name:"dev".into(),memory_gib:4,cores:1,disk_gib:10,ssh_public_key:"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into()}}).unwrap();
        let observed = dir.path().join("observed.json");
        let host = Arc::new(Simulator::new(observed.clone()));
        let vm = store.vm(op.vm_id).unwrap();
        store.operation_phase(op.id, "applying", None).unwrap();
        host.request(HostRequest::Apply {
            vm,
            vms: store.vms().unwrap(),
            users: store.users().unwrap(),
            mutation: op.request.clone(),
        })
        .await
        .unwrap();
        // Simulate death after the external side effect and before DB completion.
        drop(store);
        let restored = Store::open(path, cfg.limits).unwrap();
        let service = Service::new(restored, host).unwrap();
        service.reconcile_once().await.unwrap();
        assert!(service.store.pending().unwrap().is_empty());
        let objects: HashMap<String, serde_json::Value> =
            serde_json::from_slice(&std::fs::read(observed).unwrap()).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(service.store.vms().unwrap().len(), 1);
    }
    fn dev_spec(name: &str) -> VmSpec {
        VmSpec {
            applications: vec![],
            name: name.into(),
            memory_gib: 4,
            cores: 1,
            disk_gib: 10,
            ssh_public_key:
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                    .into(),
        }
    }
    #[tokio::test]
    async fn gpu_conflict_is_structured_and_cpu_only_consent_is_a_fresh_start() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
        let (user, token) = store.bootstrap("Admin").unwrap();
        store.enroll(&token, "device").unwrap();
        store
            .grant("device", user.id, store.limits().capacity, true)
            .unwrap();
        let mut vms = vec![];
        for name in ["a", "b"] {
            let create = store
                .submit(
                    "device",
                    uuid::Uuid::new_v4(),
                    Mutation::Create {
                        spec: dev_spec(name),
                    },
                )
                .unwrap();
            store
                .complete(create.id, "running".into(), None, false)
                .unwrap();
            let stop = store
                .submit(
                    "device",
                    uuid::Uuid::new_v4(),
                    Mutation::State {
                        vm_id: create.vm_id,
                        desired: Lifecycle::Stopped,
                    },
                )
                .unwrap();
            store
                .complete(stop.id, "stopped".into(), None, true)
                .unwrap();
            let preference = store
                .submit(
                    "device",
                    uuid::Uuid::new_v4(),
                    Mutation::Gpu {
                        vm_id: create.vm_id,
                        pci: Some("0000:01:00.0".into()),
                    },
                )
                .unwrap();
            store
                .complete(preference.id, "stopped".into(), None, true)
                .unwrap();
            vms.push(create.vm_id);
        }
        let service = Service::new(
            store,
            Arc::new(Simulator::new(dir.path().join("observed.json"))),
        )
        .unwrap();
        let request = |vm_id, without_gpu| Request::Mutate {
            idempotency_key: uuid::Uuid::new_v4(),
            mutation: Mutation::Start { vm_id, without_gpu },
        };
        assert!(matches!(
            service
                .request("device".into(), request(vms[0], false))
                .await
                .unwrap(),
            Response::Operation(_)
        ));
        let response = service
            .request("device".into(), request(vms[1], false))
            .await
            .unwrap();
        // Exercise wire serialization: callers branch on a response type, never
        // on a substring in a human-readable error message.
        let response: Response =
            serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert!(matches!(response, Response::GpuUnavailable(ref c) if c.vm_id == vms[1]));
        assert!(!service.store.vm(vms[1]).unwrap().has_compute());
        assert!(matches!(
            service
                .request("device".into(), request(vms[1], true))
                .await
                .unwrap(),
            Response::Operation(_)
        ));
        let vm = service.store.vm(vms[1]).unwrap();
        assert!(vm.has_compute() && vm.gpus.is_empty() && !vm.preferred_gpus.is_empty());
    }

    struct AdapterProtocol {
        simulator: Simulator,
        confirms_cleanup: std::sync::atomic::AtomicBool,
    }
    #[async_trait::async_trait]
    impl Backend for AdapterProtocol {
        async fn request(&self, request: HostRequest) -> Result<HostResponse> {
            let mut response = self.simulator.request(request).await?;
            if let HostResponse::Applied(observation) = &mut response {
                observation.compute_released &= self.confirms_cleanup.load(Ordering::SeqCst);
            }
            Ok(response)
        }
    }
    #[tokio::test]
    async fn ordinary_stopped_response_cannot_release_claims_until_adapter_confirms_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
        let (user, token) = store.bootstrap("Admin").unwrap();
        store.enroll(&token, "device").unwrap();
        store
            .grant("device", user.id, store.limits().capacity, true)
            .unwrap();
        let create = store
            .submit(
                "device",
                uuid::Uuid::new_v4(),
                Mutation::Create {
                    spec: dev_spec("dev"),
                },
            )
            .unwrap();
        store
            .complete(create.id, "running".into(), None, false)
            .unwrap();
        let stop = store
            .submit(
                "device",
                uuid::Uuid::new_v4(),
                Mutation::State {
                    vm_id: create.vm_id,
                    desired: Lifecycle::Stopped,
                },
            )
            .unwrap();
        let adapter = Arc::new(AdapterProtocol {
            simulator: Simulator::new(dir.path().join("observed.json")),
            confirms_cleanup: std::sync::atomic::AtomicBool::new(false),
        });
        let service = Service::new(store, adapter.clone()).unwrap();
        service.reconcile_once().await.unwrap();
        assert!(service.store.vm(create.vm_id).unwrap().has_compute());
        assert_eq!(
            service
                .store
                .operations()
                .unwrap()
                .into_iter()
                .find(|o| o.id == stop.id)
                .unwrap()
                .phase,
            "needs_review"
        );
        // A confirmed host stop has happened, but claims survive until the
        // upgraded adapter can attest cleanup during an explicit recovery retry.
        adapter.confirms_cleanup.store(true, Ordering::SeqCst);
        service.store.retry_operation("device", stop.id).unwrap();
        service.reconcile_once().await.unwrap();
        assert!(!service.store.vm(create.vm_id).unwrap().has_compute());
        assert_eq!(
            service.store.vm(create.vm_id).unwrap().resources.disk_bytes,
            12 * GIB
        );
    }
    #[tokio::test]
    async fn vm_deletion_finishes_pending_application_work_without_holding_resources() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
        let (user, token) = store.bootstrap("Operator").unwrap();
        store.enroll(&token, "device").unwrap();
        store
            .grant("device", user.id, store.limits().capacity, false)
            .unwrap();
        let mut spec = dev_spec("app deletion");
        spec.applications = vec![ApplicationKind::T3];
        let create = store
            .submit("device", uuid::Uuid::new_v4(), Mutation::Create { spec })
            .unwrap();
        let service = Service::new(
            store.clone(),
            Arc::new(Simulator::new(dir.path().join("observed.json"))),
        )
        .unwrap();
        service.reconcile_once().await.unwrap();
        store
            .submit(
                "device",
                uuid::Uuid::new_v4(),
                Mutation::State {
                    vm_id: create.vm_id,
                    desired: Lifecycle::Deleted,
                },
            )
            .unwrap();
        service.reconcile_once().await.unwrap();
        service.reconcile_applications_once().await.unwrap();
        assert_eq!(
            store.application_operations().unwrap()[0].phase,
            ApplicationPhase::Interrupted
        );
        assert_eq!(
            store.vm(create.vm_id).unwrap().resources,
            Resources::default()
        );
    }
    #[tokio::test]
    async fn codex_authorization_challenge_is_only_in_owners_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
        let (owner, token) = store.bootstrap("Owner").unwrap();
        store.enroll(&token, "owner").unwrap();
        store
            .grant("owner", owner.id, store.limits().capacity, false)
            .unwrap();
        let (token, _, _) = store
            .invite(
                "owner",
                "Other administrator",
                Role::Admin,
                Resources::default(),
                false,
                1,
            )
            .unwrap();
        store.enroll(&token, "other-admin").unwrap();
        let mut spec = dev_spec("owned");
        spec.applications = vec![ApplicationKind::T3];
        let create = store
            .submit("owner", uuid::Uuid::new_v4(), Mutation::Create { spec })
            .unwrap();
        store
            .complete(create.id, "running".into(), None, false)
            .unwrap();
        let app = store.applications().unwrap().remove(0);
        store
            .record_application_report(
                app.id,
                ApplicationReport {
                    observation: ApplicationObservation {
                        accounts: vec![ApplicationAccount {
                            name: "Codex CLI".into(),
                            email: Some("owner@example.com".into()),
                            plan: Some("pro".into()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    job: None,
                },
            )
            .unwrap();
        let service = Service::new(
            store,
            Arc::new(Simulator::new(dir.path().join("host.json"))),
        )
        .unwrap();
        service.application_actions.lock().await.insert(
            app.id,
            BrowserAction {
                url: "https://auth.openai.com/codex/device".into(),
                code: "PRIVATE-CODE".into(),
                expires_at: homecloud_store::now() + 600,
                setup_action: ApplicationSetupAction::SignInCodex,
                input_required: false,
            },
        );
        for (peer, visible) in [("owner", true), ("other-admin", false)] {
            let Response::Snapshot(snapshot) = service
                .request(peer.into(), Request::Snapshot)
                .await
                .unwrap()
            else {
                panic!("Expected snapshot")
            };
            assert_eq!(
                snapshot.disk_overhead_gib_per_vm,
                service.store.limits().disk_overhead_gib_per_vm
            );
            assert_eq!(
                snapshot.applications[0].observation.action.is_some(),
                visible
            );
            assert_eq!(
                !snapshot.applications[0].observation.accounts.is_empty(),
                visible
            );
            if visible {
                assert_eq!(
                    snapshot.applications[0].observation.accounts[0]
                        .email
                        .as_deref(),
                    Some("owner@example.com")
                );
            }
        }
    }
    #[tokio::test]
    async fn provider_code_delivery_requires_owner_matching_job_and_live_challenge() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
        let (owner, token) = store.bootstrap("Owner").unwrap();
        store.enroll(&token, "owner").unwrap();
        store
            .grant("owner", owner.id, store.limits().capacity, false)
            .unwrap();
        let (token, _, _) = store
            .invite(
                "owner",
                "Other admin",
                Role::Admin,
                Resources::default(),
                false,
                1,
            )
            .unwrap();
        store.enroll(&token, "other").unwrap();
        let mut spec = dev_spec("code delivery");
        spec.applications = vec![ApplicationKind::T3];
        let create = store
            .submit("owner", uuid::Uuid::new_v4(), Mutation::Create { spec })
            .unwrap();
        store
            .complete(create.id, "running".into(), None, false)
            .unwrap();
        let app = store.applications().unwrap().remove(0);
        let install = store.application_operations().unwrap().remove(0);
        let observation = ApplicationObservation {
            installed_version: Some("0.1.2".into()),
            setup: "ready".into(),
            components: vec![ApplicationComponent {
                name: "Claude Code".into(),
                version: Some("2.1.308".into()),
                ready: true,
                account: "signed_out".into(),
            }],
            ..Default::default()
        };
        store
            .record_application_report(
                app.id,
                ApplicationReport {
                    observation: observation.clone(),
                    job: Some(ApplicationJob {
                        id: install.id,
                        phase: ApplicationPhase::Succeeded,
                    }),
                },
            )
            .unwrap();
        let op = store
            .start_application_setup(
                "owner",
                app.id,
                uuid::Uuid::new_v4(),
                ApplicationSetupAction::SignInClaude,
            )
            .unwrap();
        store
            .record_application_report(
                app.id,
                ApplicationReport {
                    observation: ApplicationObservation {
                        operation_id: Some(op.id),
                        ..observation
                    },
                    job: Some(ApplicationJob {
                        id: op.id,
                        phase: ApplicationPhase::WaitingForUser,
                    }),
                },
            )
            .unwrap();
        let service = Service::new(
            store,
            Arc::new(Simulator::new(dir.path().join("host.json"))),
        )
        .unwrap();
        let request = |id| Request::SubmitApplicationCode {
            application_id: app.id,
            operation_id: id,
            code: AuthorizationCode("FIXTURE-RETURN-CODE".into()),
        };
        assert!(
            service
                .request("owner".into(), request(op.id))
                .await
                .is_err()
        );
        service.application_actions.lock().await.insert(
            app.id,
            BrowserAction {
                url: "https://claude.com/cai/oauth/authorize".into(),
                code: String::new(),
                expires_at: homecloud_store::now() + 600,
                setup_action: ApplicationSetupAction::SignInClaude,
                input_required: true,
            },
        );
        assert!(
            service
                .request("other".into(), request(op.id))
                .await
                .is_err()
        );
        assert!(
            service
                .request("owner".into(), request(install.id))
                .await
                .is_err()
        );
        assert!(matches!(
            service
                .request("owner".into(), request(op.id))
                .await
                .unwrap(),
            Response::Ok
        ));
        let Response::Snapshot(snapshot) = service
            .request("other".into(), Request::Snapshot)
            .await
            .unwrap()
        else {
            panic!("Expected snapshot")
        };
        assert!(snapshot.applications[0].observation.action.is_none());
        service
            .application_actions
            .lock()
            .await
            .get_mut(&app.id)
            .unwrap()
            .expires_at = 0;
        assert!(
            service
                .request("owner".into(), request(op.id))
                .await
                .is_err()
        );
    }
    struct FailingStopBackend {
        simulator: Simulator,
        graceful: AtomicUsize,
        forced: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Backend for FailingStopBackend {
        async fn request(&self, request: HostRequest) -> Result<HostResponse> {
            if let HostRequest::Apply { mutation, .. } = &request
                && mutation.stops_vm()
            {
                if matches!(mutation, Mutation::ForceStop { .. }) {
                    self.forced.fetch_add(1, Ordering::SeqCst);
                } else {
                    self.graceful.fetch_add(1, Ordering::SeqCst);
                }
                anyhow::bail!("injected adapter failure without conflict prefix");
            }
            self.simulator.request(request).await
        }
    }
    #[tokio::test]
    async fn failed_shutdown_requires_explicit_recovery_and_never_auto_escalates() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
        let (owner, token) = store.bootstrap("Admin").unwrap();
        store.enroll(&token, "device").unwrap();
        store
            .grant("device", owner.id, store.limits().capacity, false)
            .unwrap();
        let create = store
            .submit(
                "device",
                uuid::Uuid::new_v4(),
                Mutation::Create {
                    spec: dev_spec("shutdown"),
                },
            )
            .unwrap();
        store
            .complete(create.id, "running".into(), None, false)
            .unwrap();
        let stop = store
            .submit(
                "device",
                uuid::Uuid::new_v4(),
                Mutation::State {
                    vm_id: create.vm_id,
                    desired: Lifecycle::Stopped,
                },
            )
            .unwrap();
        let backend = Arc::new(FailingStopBackend {
            simulator: Simulator::new(dir.path().join("observed.json")),
            graceful: AtomicUsize::new(0),
            forced: AtomicUsize::new(0),
        });
        let service = Service::new(store, backend.clone()).unwrap();
        service.reconcile_once().await.unwrap();
        service.reconcile_once().await.unwrap();
        assert_eq!(backend.graceful.load(Ordering::SeqCst), 1);
        assert_eq!(backend.forced.load(Ordering::SeqCst), 0);
        assert_eq!(
            service
                .store
                .operations()
                .unwrap()
                .iter()
                .find(|o| o.id == stop.id)
                .unwrap()
                .phase,
            "needs_review"
        );
        let force = service
            .store
            .submit(
                "device",
                uuid::Uuid::new_v4(),
                Mutation::ForceStop {
                    vm_id: create.vm_id,
                },
            )
            .unwrap();
        service.reconcile_once().await.unwrap();
        service.reconcile_once().await.unwrap();
        assert_eq!(backend.forced.load(Ordering::SeqCst), 1);
        assert_eq!(
            service
                .store
                .operations()
                .unwrap()
                .iter()
                .find(|o| o.id == force.id)
                .unwrap()
                .phase,
            "needs_review"
        );
        assert!(service.store.vm(create.vm_id).unwrap().has_compute());
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct DiagnosticsBackend(AtomicUsize);
    #[async_trait::async_trait]
    impl Backend for DiagnosticsBackend {
        async fn request(&self, request: HostRequest) -> Result<HostResponse> {
            match request {
                HostRequest::Diagnostics { vm } => {
                    self.0.fetch_add(1, Ordering::SeqCst);
                    Ok(HostResponse::Diagnostics(VmDiagnostics {
                        vm_id: vm.id,
                        progress: None,
                        power_state: "running".into(),
                        checks: vec![],
                        logs: vec![],
                    }))
                }
                _ => anyhow::bail!("Unexpected request"),
            }
        }
    }
    #[tokio::test]
    async fn setup_logs_are_authorized_before_guest_access_and_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
        let (admin, token) = store.bootstrap("Admin").unwrap();
        store.enroll(&token, "admin").unwrap();
        store
            .grant("admin", admin.id, store.limits().capacity, true)
            .unwrap();
        let key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let op = store
            .submit(
                "admin",
                uuid::Uuid::new_v4(),
                Mutation::Create {
                    spec: VmSpec {
                        applications: vec![],
                        name: "proof".into(),
                        memory_gib: 1,
                        cores: 1,
                        disk_gib: 10,
                        ssh_public_key: key.into(),
                    },
                },
            )
            .unwrap();
        let (token, _, _) = store
            .invite("admin", "Other", Role::User, Resources::default(), false, 1)
            .unwrap();
        store.enroll(&token, "other").unwrap();
        let backend = Arc::new(DiagnosticsBackend(AtomicUsize::new(0)));
        let service = Service::new(store, backend.clone()).unwrap();
        assert!(
            service
                .request("other".into(), Request::VmDiagnostics { vm_id: op.vm_id })
                .await
                .is_err()
        );
        assert_eq!(backend.0.load(Ordering::SeqCst), 0);
        let response = service
            .request("admin".into(), Request::VmDiagnostics { vm_id: op.vm_id })
            .await
            .unwrap();
        assert!(matches!(response, Response::VmDiagnostics(_)));
        assert_eq!(backend.0.load(Ordering::SeqCst), 1);
        service.store.revoke_device("admin", "other").unwrap();
        assert!(
            service
                .request("other".into(), Request::VmDiagnostics { vm_id: op.vm_id })
                .await
                .is_err()
        );
        assert_eq!(backend.0.load(Ordering::SeqCst), 1);
        assert_eq!(service.store.operations().unwrap().len(), 1);
    }
}
