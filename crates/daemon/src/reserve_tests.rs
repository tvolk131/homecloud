use super::*;
use std::collections::HashSet;
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};
use uuid::Uuid;

struct ReserveBackend {
    vm: homecloud_host::Simulator,
    policy: StdMutex<HostReservePolicy>,
    fail: AtomicU8,
    status_failure: AtomicBool,
    applies: AtomicUsize,
    discarded: StdMutex<HashSet<Uuid>>,
}
#[async_trait::async_trait]
impl Backend for ReserveBackend {
    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        match request {
            HostRequest::ReserveStatus => {
                ensure!(
                    !self.status_failure.load(Ordering::SeqCst),
                    "Injected unreadable policy"
                );
                Ok(HostResponse::ReservePolicy(Some(
                    self.policy.lock().unwrap().clone(),
                )))
            }
            HostRequest::ApplyReserves { operation, .. } => {
                ensure!(
                    !self.discarded.lock().unwrap().contains(&operation.id),
                    "Discarded operation"
                );
                self.applies.fetch_add(1, Ordering::SeqCst);
                let fail = self.fail.swap(0, Ordering::SeqCst);
                if fail == 1 {
                    anyhow::bail!("Injected pre-effect failure")
                }
                let mut policy = self.policy.lock().unwrap();
                policy.state = operation.target;
                policy.operation_id = Some(operation.id);
                policy.applying = fail == 3;
                if fail == 2 || fail == 3 {
                    anyhow::bail!("Injected lost acknowledgement")
                }
                Ok(HostResponse::ReservePolicy(Some(policy.clone())))
            }
            HostRequest::DiscardReserves { operation } => {
                let policy = self.policy.lock().unwrap().clone();
                ensure!(
                    policy.state == operation.before && !policy.applying,
                    "Effects have begun"
                );
                self.discarded.lock().unwrap().insert(operation.id);
                Ok(HostResponse::DiscardedReserves {
                    operation_id: operation.id,
                    policy,
                })
            }
            request => self.vm.request(request).await,
        }
    }
}
fn fixture() -> (tempfile::TempDir, Arc<Service>, Arc<ReserveBackend>, User) {
    let dir = tempfile::tempdir().unwrap();
    let limits = Config::default().limits;
    let store = Store::open(dir.path().join("state.db"), limits.clone()).unwrap();
    let (_, token) = store.bootstrap("Admin").unwrap();
    let user = store.enroll(&token, "admin").unwrap();
    store
        .grant(
            "admin",
            user.id,
            Resources {
                memory_bytes: 4 * GIB,
                cpu_cores: 1,
                disk_bytes: 32 * GIB,
            },
            false,
        )
        .unwrap();
    let backend = Arc::new(ReserveBackend {
        vm: homecloud_host::Simulator::new(dir.path().join("observed.json")),
        policy: StdMutex::new(HostReservePolicy {
            excluded_network_slots: vec![],
            memory: None,
            envelope: limits.capacity,
            core_groups: limits.core_groups,
            disk_overhead_gib_per_vm: 2,
            minimum_reserves: Resources::default(),
            arc: None,
            memory_policy: None,
            base_pool_free_bytes: 30 * GIB,
            state: HostReserves::default(),
            applying: false,
            operation_id: None,
        }),
        fail: AtomicU8::new(0),
        status_failure: AtomicBool::new(false),
        applies: AtomicUsize::new(0),
        discarded: StdMutex::new(HashSet::new()),
    });
    let service = Service::new(store, backend.clone()).unwrap();
    (dir, service, backend, user)
}
async fn set(service: &Service) -> HostReserveOperation {
    match service
        .request(
            "admin".into(),
            Request::SetHostReserves {
                arc: None,
                reserves: Resources {
                    memory_bytes: 8 * GIB,
                    cpu_cores: 1,
                    disk_bytes: 64 * GIB,
                },
                expected_revision: 0,
                idempotency_key: Uuid::new_v4(),
            },
        )
        .await
        .unwrap()
    {
        Response::HostReserveOperation(op) => op,
        _ => panic!("Missing operation"),
    }
}
#[tokio::test]
async fn reserve_rpc_is_admin_only_and_member_snapshots_omit_policy_metadata() {
    let (_dir, s, backend, admin) = fixture();
    let (token, _, _) = s
        .store
        .invite(
            "admin",
            "Coworker",
            Role::User,
            Resources::default(),
            false,
            24,
        )
        .unwrap();
    s.store.enroll(&token, "member").unwrap();
    assert!(
        s.request(
            "member".into(),
            Request::SetHostReserves {
                arc: None,
                reserves: Resources::default(),
                expected_revision: 0,
                idempotency_key: Uuid::new_v4()
            }
        )
        .await
        .is_err()
    );
    let op = set(&s).await;
    assert_eq!(s.store.capacity().unwrap().cpu_cores, 1);
    s.reconcile_once().await.unwrap();
    assert_eq!(backend.applies.load(Ordering::SeqCst), 1);
    assert_eq!(s.store.host_reserve_state().unwrap(), op.target);
    assert_eq!(
        s.store
            .users()
            .unwrap()
            .iter()
            .find(|u| u.id == admin.id)
            .unwrap()
            .grant
            .cpu_cores,
        1
    );
    match s.request("member".into(), Request::Snapshot).await.unwrap() {
        Response::Snapshot(snapshot) => assert!(snapshot.host_reserves.is_none()),
        _ => panic!("Missing snapshot"),
    }
    match s.request("admin".into(), Request::Snapshot).await.unwrap() {
        Response::Snapshot(snapshot) => assert_eq!(
            snapshot.host_reserves.unwrap().operation.unwrap().phase,
            ReservePhase::Succeeded
        ),
        _ => panic!("Missing snapshot"),
    }
}
#[tokio::test]
async fn lost_acknowledgement_converges_from_host_proof_without_reapplying() {
    let (_dir, s, backend, _) = fixture();
    backend.fail.store(2, Ordering::SeqCst);
    let op = set(&s).await;
    assert!(s.reconcile_once().await.is_err());
    assert_eq!(
        s.store.host_reserve_operation().unwrap().unwrap().phase,
        ReservePhase::NeedsReview
    );
    s.reconcile_once().await.unwrap();
    assert_eq!(s.store.host_reserve_state().unwrap(), op.target);
    assert_eq!(backend.applies.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn incomplete_host_effect_blocks_growth_and_recovers_after_controller_restart() {
    let (dir, s, backend, admin) = fixture();
    backend.fail.store(3, Ordering::SeqCst);
    let op = set(&s).await;
    assert!(s.reconcile_once().await.is_err());
    drop(s);
    let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
    let s = Service::new(store, backend.clone()).unwrap();
    assert!(
        s.request(
            "admin".into(),
            Request::Grant {
                user_id: admin.id,
                grant: Resources::default(),
                gpu_allowed: false
            }
        )
        .await
        .is_err()
    );
    assert!(s.reconcile_once().await.is_err());
    s.request(
        "admin".into(),
        Request::RetryHostReserves {
            operation_id: op.id,
        },
    )
    .await
    .unwrap();
    s.reconcile_once().await.unwrap();
    assert_eq!(s.store.host_reserve_state().unwrap(), op.target);
    assert_eq!(backend.applies.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn discard_requires_a_durable_host_tombstone_before_releasing_the_fence() {
    let (_dir, s, backend, _) = fixture();
    backend.fail.store(1, Ordering::SeqCst);
    let op = set(&s).await;
    assert!(s.reconcile_once().await.is_err());
    s.request(
        "admin".into(),
        Request::DiscardHostReserves {
            operation_id: op.id,
        },
    )
    .await
    .unwrap();
    assert!(backend.discarded.lock().unwrap().contains(&op.id));
    assert_eq!(
        s.store.capacity().unwrap(),
        Config::default().limits.capacity
    );
    assert!(
        backend
            .request(HostRequest::ApplyReserves {
                operation: op,
                vms: vec![],
                users: vec![]
            })
            .await
            .is_err()
    );
}
#[tokio::test]
async fn unexpected_or_missing_host_policy_fails_admission_closed() {
    let (dir, s, backend, admin) = fixture();
    s.reserve_policy().await.unwrap();
    backend.policy.lock().unwrap().state.revision = 99;
    let before = s.store.users().unwrap()[0].grant;
    assert!(
        s.request(
            "admin".into(),
            Request::Grant {
                user_id: admin.id,
                grant: Resources::default(),
                gpu_allowed: false
            }
        )
        .await
        .is_err()
    );
    assert_eq!(s.store.users().unwrap()[0].grant, before);
    assert!(s.reconcile_once().await.is_err());
    // Once supported, silently downgrading to an adapter without policy support
    // cannot bypass the persisted feature gate, even at reserve revision zero.
    drop(s);
    let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
    let legacy = Service::new(
        store,
        Arc::new(homecloud_host::Simulator::new(
            dir.path().join("legacy.json"),
        )),
    )
    .unwrap();
    assert!(
        legacy
            .request(
                "admin".into(),
                Request::Grant {
                    user_id: admin.id,
                    grant: Resources::default(),
                    gpu_allowed: false
                }
            )
            .await
            .is_err()
    );
    assert!(legacy.reconcile_once().await.is_err());
    drop(backend);
}

#[tokio::test]
async fn unreadable_policy_blocks_admission_even_before_the_first_reserve_edit() {
    let (_dir, s, backend, admin) = fixture();
    backend.status_failure.store(true, Ordering::SeqCst);
    let before = s.store.users().unwrap()[0].grant;
    assert!(
        s.request(
            "admin".into(),
            Request::Grant {
                user_id: admin.id,
                grant: Resources::default(),
                gpu_allowed: false
            }
        )
        .await
        .is_err()
    );
    assert_eq!(s.store.users().unwrap()[0].grant, before);
    assert!(s.reconcile_once().await.is_err());
}

#[tokio::test]
async fn total_reserves_convert_from_explicit_minimum_and_cache_is_independent() {
    let (_dir, s, b, _admin) = fixture();
    let minimum = Resources {
        memory_bytes: 4 * GIB,
        cpu_cores: 1,
        disk_bytes: 30 * GIB,
    };
    b.policy.lock().unwrap().minimum_reserves = minimum;
    b.policy.lock().unwrap().memory_policy = Some(HostMemoryPolicy {
        operating_floor_bytes: 2 * GIB,
        recommended_arc_bytes: 2 * GIB,
        minimum_system_bytes: 4 * GIB,
    });
    let request = |reserves, arc| Request::SetHostReserves {
        reserves,
        arc,
        expected_revision: 0,
        idempotency_key: Uuid::new_v4(),
    };
    assert!(
        s.request("admin".into(), request(Resources::default(), None))
            .await
            .is_err()
    );
    let capacity = s.store.capacity().unwrap();
    let op = match s
        .request("admin".into(), request(minimum, Some(ArcMode::Automatic)))
        .await
        .unwrap()
    {
        Response::HostReserveOperation(op) => op,
        _ => panic!("Expected reserve operation"),
    };
    assert_eq!(op.target.additional, Resources::default());
    assert_eq!(op.target.arc, Some(ArcMode::Automatic));
    assert_eq!(s.store.capacity().unwrap(), capacity);
    s.reconcile_once().await.unwrap();
    assert_eq!(
        s.store.host_reserve_state().unwrap().arc,
        Some(ArcMode::Automatic)
    );
}

#[tokio::test]
async fn arc_mode_survives_controller_restart_after_a_lost_host_acknowledgement() {
    let (dir, s, backend, _) = fixture();
    backend.policy.lock().unwrap().minimum_reserves.memory_bytes = 4 * GIB;
    backend.policy.lock().unwrap().memory_policy = Some(HostMemoryPolicy {
        operating_floor_bytes: 2 * GIB,
        recommended_arc_bytes: 2 * GIB,
        minimum_system_bytes: 4 * GIB,
    });
    backend.fail.store(2, Ordering::SeqCst);
    let capacity = s.store.capacity().unwrap();
    let op = match s
        .request(
            "admin".into(),
            Request::SetHostReserves {
                reserves: Resources {
                    memory_bytes: 4 * GIB,
                    ..Resources::default()
                },
                arc: Some(ArcMode::Automatic),
                expected_revision: 0,
                idempotency_key: Uuid::new_v4(),
            },
        )
        .await
        .unwrap()
    {
        Response::HostReserveOperation(op) => op,
        _ => panic!("Expected operation"),
    };
    assert!(s.reconcile_once().await.is_err());
    drop(s);
    let store = Store::open(dir.path().join("state.db"), Config::default().limits).unwrap();
    let service = Service::new(store, backend.clone()).unwrap();
    service.reconcile_once().await.unwrap();
    assert_eq!(service.store.host_reserve_state().unwrap(), op.target);
    assert_eq!(service.store.capacity().unwrap(), capacity);
    assert_eq!(backend.applies.load(Ordering::SeqCst), 1);
}
