//! Event-driven host contract model. SQLite is real; host effects and replies
//! advance separately, without sleeps or a pretend ZFS/QEMU implementation.
use super::*;
use std::sync::Mutex as StdMutex;
use uuid::Uuid;

#[derive(Default)]
struct Effects {
    qemu_alive: bool,
    power: &'static str,
    cleanup: bool,
    launches: usize,
    lost_launch_reply: bool,
    lost_capacity_reply: bool,
    capacity_applies: usize,
    capacity_receipts: Vec<Uuid>,
    capacity_cancelled: Vec<Uuid>,
}
struct Scenario {
    policy: StdMutex<HostReservePolicy>,
    effects: StdMutex<Effects>,
}
impl Scenario {
    fn new() -> Self {
        let c = Config::default();
        Self {
            policy: StdMutex::new(HostReservePolicy {
                excluded_network_slots: vec![],
                memory: Some(MemoryCapacityStatus {
                    revision: 0,
                    reviewed_bytes: 32 * GIB,
                    safe_bytes: 32 * GIB,
                    physical_bytes: 36 * GIB,
                    minimum_system_bytes: 4 * GIB,
                    boot_id: Uuid::nil().to_string(),
                    observed_at: homecloud_store::now(),
                    arc_ready: true,
                    error: None,
                }),
                envelope: c.limits.capacity,
                core_groups: c.limits.core_groups,
                disk_overhead_gib_per_vm: 2,
                base_pool_free_bytes: 30 * GIB,
                state: HostReserves::default(),
                applying: false,
                operation_id: None,
                minimum_reserves: Resources::default(),
                arc: None,
                memory_policy: None,
            }),
            effects: StdMutex::new(Effects {
                power: "stopped",
                lost_launch_reply: true,
                ..Default::default()
            }),
        }
    }
}
#[async_trait::async_trait]
impl Backend for Scenario {
    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        match request {
            HostRequest::ReserveStatus => {
                let mut p = self.policy.lock().unwrap();
                p.memory.as_mut().unwrap().observed_at = homecloud_store::now();
                Ok(HostResponse::ReservePolicy(Some(p.clone())))
            }
            HostRequest::AdoptMemoryCapacity { operation: op } => {
                let mut p = self.policy.lock().unwrap();
                let mut e = self.effects.lock().unwrap();
                ensure!(
                    !e.capacity_cancelled.contains(&op.id),
                    "cancelled memory capacity request"
                );
                if !e.capacity_receipts.contains(&op.id) {
                    let m = p.memory.as_mut().unwrap();
                    ensure!(m.revision == op.expected_revision, "stale revision");
                    m.reviewed_bytes = op.target_bytes;
                    m.revision += 1;
                    p.envelope.memory_bytes = op.target_bytes;
                    e.capacity_applies += 1;
                    e.capacity_receipts.push(op.id);
                }
                if e.lost_capacity_reply {
                    e.lost_capacity_reply = false;
                    anyhow::bail!("injected reply loss after durable host capacity effect")
                }
                let mut m = p.memory.clone().unwrap();
                m.observed_at = homecloud_store::now();
                Ok(HostResponse::MemoryCapacity(m))
            }
            HostRequest::CancelMemoryCapacity { operation: op } => {
                let mut e = self.effects.lock().unwrap();
                ensure!(
                    !e.capacity_receipts.contains(&op.id),
                    "capacity already applied"
                );
                e.capacity_cancelled.push(op.id);
                Ok(HostResponse::MemoryCapacityCancelled {
                    operation_id: op.id,
                    status: self.policy.lock().unwrap().memory.clone().unwrap(),
                })
            }
            HostRequest::Backing { .. } => Ok(HostResponse::Ok),
            HostRequest::Apply { vm, vms, .. } => {
                let mut e = self.effects.lock().unwrap();
                if vm.desired == Lifecycle::Running {
                    let p = self.policy.lock().unwrap();
                    let m = p.memory.as_ref().unwrap();
                    ensure!(
                        vms.iter().map(|v| v.resources.memory_bytes).sum::<u64>() <= m.safe_bytes,
                        "conflict: physical memory deficit"
                    );
                    if !e.qemu_alive {
                        e.qemu_alive = true;
                        e.power = "starting";
                        e.launches += 1;
                    }
                    if e.lost_launch_reply {
                        e.lost_launch_reply = false;
                        anyhow::bail!("injected lost start acknowledgement")
                    }
                    ensure!(
                        e.power == "running",
                        "pending: existing QEMU is still starting"
                    );
                    Ok(HostResponse::Applied(homecloud_host::Observation {
                        state: "running".into(),
                        ssh_host_key: None,
                        compute_released: false,
                    }))
                } else {
                    if e.qemu_alive {
                        e.power = "stopping";
                        anyhow::bail!("graceful stop has not finished")
                    }
                    ensure!(e.cleanup, "CPU/GPU cleanup has not finished");
                    Ok(HostResponse::Applied(homecloud_host::Observation {
                        state: "stopped".into(),
                        ssh_host_key: None,
                        compute_released: true,
                    }))
                }
            }
            HostRequest::Observe { vms } => {
                let e = self.effects.lock().unwrap();
                Ok(HostResponse::Observations(
                    vms.into_iter()
                        .map(|v| homecloud_host::VmObservation {
                            id: v.id,
                            state: if e.power == "running" {
                                "running"
                            } else {
                                "stopped"
                            }
                            .into(),
                            error: None,
                            progress: None,
                        })
                        .collect(),
                ))
            }
            HostRequest::Inventory => Ok(HostResponse::Inventory(HostInventory::default())),
            _ => anyhow::bail!("Unmodeled host request"),
        }
    }
}
fn fixture() -> (tempfile::TempDir, Arc<Service>, Arc<Scenario>, User) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
    let (u, t) = s.bootstrap("Owner").unwrap();
    s.enroll(&t, "owner").unwrap();
    s.grant(
        "owner",
        u.id,
        Resources {
            memory_bytes: 8 * GIB,
            cpu_cores: 2,
            disk_bytes: 128 * GIB,
        },
        false,
    )
    .unwrap();
    let backend = Arc::new(Scenario::new());
    let service = Service::new(s, backend.clone()).unwrap();
    (dir, service, backend, u)
}
#[tokio::test]
async fn deferred_host_effects_do_not_duplicate_launch_or_release_unverified_claims() {
    let (_dir, s, b, u) = fixture();
    let Response::Operation(create)=s.request("owner".into(),Request::CreateVm { idempotency_key:Uuid::new_v4(),spec:VmSpec{applications:vec![],name:"Scenario".into(),memory_gib:4,cores:1,disk_gib:10,ssh_public_key:"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into()} }).await.unwrap() else {panic!()};
    s.reconcile_once().await.unwrap();
    s.reconcile_once().await.unwrap();
    assert_eq!(b.effects.lock().unwrap().launches, 1);
    assert_eq!(
        s.store.vm(create.vm_id).unwrap().resources.memory_bytes,
        4 * GIB
    );
    b.effects.lock().unwrap().power = "running";
    s.reconcile_once().await.unwrap();
    // Physical RAM disappears; status polling cannot discard commitments.
    {
        let mut p = b.policy.lock().unwrap();
        let m = p.memory.as_mut().unwrap();
        m.safe_bytes = 2 * GIB;
        m.physical_bytes = 6 * GIB;
    }
    let Response::Operation(stop) = s
        .request(
            "owner".into(),
            Request::Mutate {
                idempotency_key: Uuid::new_v4(),
                mutation: Mutation::State {
                    vm_id: create.vm_id,
                    desired: Lifecycle::Stopped,
                },
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    s.reconcile_once().await.unwrap();
    assert_eq!(
        s.store.vm(create.vm_id).unwrap().resources.memory_bytes,
        4 * GIB
    );
    {
        let mut e = b.effects.lock().unwrap();
        e.qemu_alive = false;
        e.power = "stopped";
    }
    s.store.retry_operation("owner", stop.id).unwrap();
    s.reconcile_once().await.unwrap();
    assert_eq!(
        s.store
            .users()
            .unwrap()
            .iter()
            .find(|v| v.id == u.id)
            .unwrap()
            .committed
            .memory_bytes,
        4 * GIB
    );
    b.effects.lock().unwrap().cleanup = true;
    s.store.retry_operation("owner", stop.id).unwrap();
    s.reconcile_once().await.unwrap();
    assert_eq!(s.store.vm(create.vm_id).unwrap().resources.memory_bytes, 0);
}
#[tokio::test]
async fn lost_capacity_reply_recovers_across_controller_restart_without_reapplying() {
    let (dir, s, b, _) = fixture();
    {
        let mut p = b.policy.lock().unwrap();
        let m = p.memory.as_mut().unwrap();
        m.safe_bytes = 60 * GIB;
        m.physical_bytes = 64 * GIB;
    }
    b.effects.lock().unwrap().lost_capacity_reply = true;
    assert!(
        s.request(
            "owner".into(),
            Request::AdoptMemoryCapacity {
                expected_revision: 0,
                target_bytes: 60 * GIB,
                idempotency_key: Uuid::new_v4()
            }
        )
        .await
        .is_err()
    );
    assert!(s.store.pending_memory_capacity().unwrap().is_some());
    drop(s);
    let s = Service::new(
        Store::open(dir.path().join("state.db"), Config::default().limits).unwrap(),
        b.clone(),
    )
    .unwrap();
    s.reconcile_once().await.unwrap();
    assert_eq!(s.store.capacity().unwrap().memory_bytes, 60 * GIB);
    assert!(s.store.pending_memory_capacity().unwrap().is_none());
    assert_eq!(b.effects.lock().unwrap().capacity_applies, 1);
}

#[tokio::test]
async fn cancelling_an_unapplied_capacity_update_fences_a_delayed_backend_request() {
    let (_dir, s, b, _) = fixture();
    s.reserve_policy().await.unwrap();
    let id = Uuid::new_v4();
    let op = s
        .store
        .memory_capacity_request("owner", id, 0, 16 * GIB)
        .unwrap();
    assert_eq!(s.store.capacity().unwrap().memory_bytes, 16 * GIB);
    s.request(
        "owner".into(),
        Request::CancelMemoryCapacity { operation_id: id },
    )
    .await
    .unwrap();
    assert_eq!(s.store.capacity().unwrap().memory_bytes, 32 * GIB);
    assert!(
        b.request(HostRequest::AdoptMemoryCapacity { operation: op })
            .await
            .is_err()
    );
    assert!(s.store.pending_memory_capacity().unwrap().is_none());
}

#[tokio::test]
async fn member_snapshots_receive_startup_condition_without_other_users_or_aggregate_claims() {
    let (_dir, s, b, _) = fixture();
    let (token, _, _) = s
        .store
        .invite(
            "owner",
            "Member",
            Role::User,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    let member = s.store.enroll(&token, "member").unwrap();
    b.policy.lock().unwrap().memory.as_mut().unwrap().safe_bytes = 4 * GIB;
    let Response::Snapshot(admin) = s.request("owner".into(), Request::Snapshot).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(
        admin.memory_health.as_ref().unwrap().condition,
        MemoryCondition::PromisesExceedCapacity
    );
    assert_eq!(
        admin
            .memory_health
            .as_ref()
            .unwrap()
            .details
            .as_ref()
            .unwrap()
            .promised_bytes,
        8 * GIB
    );
    let Response::Snapshot(snapshot) = s.request("member".into(), Request::Snapshot).await.unwrap()
    else {
        panic!()
    };
    let health = snapshot.memory_health.as_ref().unwrap();
    assert_eq!(health.condition, MemoryCondition::PromisesExceedCapacity);
    assert!(health.details.is_none());
    assert!(snapshot.host_reserves.is_none());
    assert!(snapshot.vms.is_empty());
    assert_eq!(snapshot.users.len(), 1);
    assert_eq!(snapshot.users[0].id, member.id);
    assert!(
        serde_json::to_value(health)
            .unwrap()
            .get("details")
            .is_none()
    );
}

#[tokio::test]
async fn arc_drift_blocks_new_claims_but_allows_explicit_repair_unless_an_operation_is_applying() {
    for applying in [false, true] {
        let (_dir, s, b, _) = fixture();
        {
            let mut p = b.policy.lock().unwrap();
            p.memory.as_mut().unwrap().arc_ready = false;
            p.applying = applying;
        }
        assert!(s.check_reserve_admission().await.is_err());
        assert!(s.reconcile_host_reserves().await.is_err());
        let result = s
            .request(
                "owner".into(),
                Request::SetHostReserves {
                    reserves: Resources::default(),
                    arc: None,
                    expected_revision: 0,
                    idempotency_key: Uuid::new_v4(),
                },
            )
            .await;
        assert_eq!(result.is_ok(), !applying);
        assert_eq!(
            s.store.host_reserve_operation().unwrap().is_some(),
            !applying
        );
        assert_eq!(s.store.users().unwrap()[0].committed.memory_bytes, 0);
    }
}
