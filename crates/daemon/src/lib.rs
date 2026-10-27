use anyhow::{Result, ensure};
use homecloud_client::{read_frame, write_frame};
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
    sync::{Mutex, Semaphore},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub limits: Limits,
    pub host_script: PathBuf,
    #[serde(default = "default_true")]
    pub sudo: bool,
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
        }
    }
}

pub struct Service {
    pub store: Store,
    pub backend: Arc<dyn Backend>,
    enrollment_attempts: Mutex<HashMap<String, (Instant, u8)>>,
}
impl Service {
    pub fn new(store: Store, backend: Arc<dyn Backend>) -> Arc<Self> {
        Arc::new(Self {
            store,
            backend,
            enrollment_attempts: Mutex::new(HashMap::new()),
        })
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
        match request {
            Request::Snapshot => {
                let users = self.db(|s| s.users()).await?;
                let p = peer.clone();
                let devices = self.db(move |s| s.devices(&p)).await?;
                let vms = self
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
                let inventory = match self.backend.request(HostRequest::Inventory).await {
                    Ok(HostResponse::Inventory(inventory)) => inventory,
                    Err(error) => HostInventory {
                        hostname: "Host inventory unavailable".into(),
                        warnings: vec![error.to_string()],
                        ..Default::default()
                    },
                    _ => HostInventory::default(),
                };
                let users = users
                    .into_iter()
                    .filter(|u| me.role == Role::Admin || u.id == me.id)
                    .collect();
                Ok(Response::Snapshot(Box::new(Snapshot {
                    me,
                    users,
                    devices,
                    vms,
                    operations,
                    inventory,
                    capacity: self.store.limits().capacity,
                })))
            }
            Request::Invite {
                name,
                role,
                grant,
                gpu_allowed,
                expires_hours,
            } => {
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
            Request::Mutate {
                idempotency_key,
                mutation,
            } => Ok(Response::Operation(
                self.db(move |s| s.submit(&peer, idempotency_key, mutation))
                    .await?,
            )),
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
            let id = op.id;
            self.db(move |s| s.operation_phase(id, "applying", None))
                .await?;
            let vm_id = op.vm_id;
            let vm = self.db(move |s| s.vm(vm_id)).await?;
            let vms = self.db(|s| s.vms()).await?;
            match self
                .backend
                .request(HostRequest::Apply {
                    vm,
                    vms,
                    users: users.clone(),
                    mutation: op.request.clone(),
                })
                .await
            {
                Ok(HostResponse::Applied(observation)) => {
                    self.db(move |s| s.complete(id, observation.state, observation.ssh_host_key))
                        .await?;
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
                    let phase = if message.starts_with("conflict:") {
                        "needs_review"
                    } else {
                        "retrying"
                    };
                    self.db(move |s| s.operation_phase(id, phase, Some(message)))
                        .await?;
                }
            }
        }
        let vms = self.db(|s| s.vms()).await?;
        if let HostResponse::Observations(observed) =
            self.backend.request(HostRequest::Observe { vms }).await?
        {
            for vm in observed {
                self.db(move |s| s.observe(vm.id, vm.state, vm.error))
                    .await?;
            }
        }
        Ok(())
    }
    async fn connection(self: Arc<Self>, conn: Connection) -> Result<()> {
        let peer = conn.remote_id().to_string();
        let (mut send, mut recv) =
            tokio::time::timeout(Duration::from_secs(15), conn.accept_bi()).await??;
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
        let op=store.submit("device",uuid::Uuid::new_v4(),Mutation::Create{spec:VmSpec{name:"dev".into(),memory_gib:4,cores:1,disk_gib:10,ssh_public_key:"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into()}}).unwrap();
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
        let service = Service::new(restored, host);
        service.reconcile_once().await.unwrap();
        assert!(service.store.pending().unwrap().is_empty());
        let objects: HashMap<String, serde_json::Value> =
            serde_json::from_slice(&std::fs::read(observed).unwrap()).unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(service.store.vms().unwrap().len(), 1);
    }
}
