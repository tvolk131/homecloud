use homecloud_core::*;
use homecloud_daemon::{Config, Service};
use homecloud_store::Store;
use homecloud_virtualization::{
    PowerSubmission, PowerTaskState, SimulatedVirtualization, VirtualizationBackend,
};
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};
use uuid::Uuid;

struct PowerBackend {
    inner: SimulatedVirtualization,
    submissions: AtomicUsize,
    outcome: AtomicU8,
    task_state: AtomicU8,
}

impl PowerBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: SimulatedVirtualization::demo(),
            submissions: AtomicUsize::new(0),
            outcome: AtomicU8::new(0),
            task_state: AtomicU8::new(0),
        })
    }
}

#[async_trait::async_trait]
impl VirtualizationBackend for PowerBackend {
    async fn inventory(&self) -> anyhow::Result<VirtualizationInventory> {
        self.inner.inventory().await
    }
    async fn submit_power(
        &self,
        guest: &VirtualizationGuestIdentity,
        action: VirtualizationPowerAction,
    ) -> PowerSubmission {
        self.submissions.fetch_add(1, Ordering::SeqCst);
        match self.outcome.load(Ordering::SeqCst) {
            1 => PowerSubmission::Rejected("HTTP 403: VM power permission required".into()),
            2 => PowerSubmission::Unknown("Response lost after dispatch".into()),
            _ => self.inner.submit_power(guest, action).await,
        }
    }
    async fn power_task(
        &self,
        _: &VirtualizationGuestIdentity,
        _: VirtualizationPowerAction,
        _: &str,
    ) -> anyhow::Result<PowerTaskState> {
        match self.task_state.load(Ordering::SeqCst) {
            0 => Ok(PowerTaskState::Running),
            1 => Ok(PowerTaskState::Succeeded),
            2 => Ok(PowerTaskState::Failed),
            _ => anyhow::bail!("Task status temporarily unavailable"),
        }
    }
}

fn setup() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(
        directory.path().join("virtualization.db"),
        Config::default().limits,
    )
    .unwrap();
    let (_, token) = store.bootstrap("Owner").unwrap();
    store.enroll(&token, "owner").unwrap();
    let (token, _, _) = store
        .invite(
            "owner",
            "Member",
            Role::User,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    store.enroll(&token, "member").unwrap();
    (directory, store)
}

fn guest() -> VirtualizationGuestIdentity {
    VirtualizationGuestIdentity {
        node: "demo-proxmox".into(),
        vmid: 101,
        kind: VirtualizationGuestKind::VirtualMachine,
    }
}

fn request(key: Uuid, action: VirtualizationPowerAction) -> Request {
    Request::VirtualizationPower {
        idempotency_key: key,
        guest: guest(),
        action,
    }
}

async fn dispatched(store: &Store, id: Uuid) -> VirtualizationOperation {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let operation = store.virtualization_operation(id).unwrap().unwrap();
            if operation.phase != VirtualizationOperationPhase::Submitting {
                return operation;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn power_is_owner_only_deduplicated_and_serialized_per_guest() {
    let (_directory, store) = setup();
    let backend = PowerBackend::new();
    let service = Service::new_virtualization(store.clone(), backend.clone()).unwrap();
    for peer in ["unknown", "member"] {
        assert!(
            service
                .request(
                    peer.into(),
                    request(Uuid::new_v4(), VirtualizationPowerAction::Shutdown)
                )
                .await
                .is_err()
        );
        assert!(
            service
                .request(peer.into(), Request::VirtualizationOperations)
                .await
                .is_err()
        );
    }
    let key = Uuid::new_v4();
    let (first, duplicate) = tokio::join!(
        service.request(
            "owner".into(),
            request(key, VirtualizationPowerAction::Shutdown)
        ),
        service.request(
            "owner".into(),
            request(key, VirtualizationPowerAction::Shutdown)
        )
    );
    assert!(
        matches!(first.unwrap(), Response::VirtualizationOperation(operation) if operation.id == key)
    );
    assert!(
        matches!(duplicate.unwrap(), Response::VirtualizationOperation(operation) if operation.id == key)
    );
    assert_eq!(
        dispatched(&store, key).await.phase,
        VirtualizationOperationPhase::Running
    );
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 1);
    assert!(
        service
            .request(
                "owner".into(),
                request(key, VirtualizationPowerAction::ForceStop)
            )
            .await
            .is_err()
    );
    assert!(
        service
            .request(
                "owner".into(),
                request(Uuid::new_v4(), VirtualizationPowerAction::ForceStop)
            )
            .await
            .is_err()
    );
    backend.task_state.store(1, Ordering::SeqCst);
    service.refresh_virtualization_operations().await.unwrap();
    assert_eq!(
        store.virtualization_operation(key).unwrap().unwrap().phase,
        VirtualizationOperationPhase::Succeeded
    );
    service
        .request(
            "owner".into(),
            request(key, VirtualizationPowerAction::Shutdown),
        )
        .await
        .unwrap();
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 1);
    assert!(store.vms().unwrap().is_empty());
}

#[tokio::test]
async fn task_tracking_survives_service_restart_and_temporary_read_failure() {
    let (_directory, store) = setup();
    let backend = PowerBackend::new();
    let service = Service::new_virtualization(store.clone(), backend.clone()).unwrap();
    let key = Uuid::new_v4();
    service
        .request(
            "owner".into(),
            request(key, VirtualizationPowerAction::Shutdown),
        )
        .await
        .unwrap();
    let original = dispatched(&store, key).await;
    drop(service);
    let service = Service::new_virtualization(store.clone(), backend.clone()).unwrap();
    backend.task_state.store(3, Ordering::SeqCst);
    service.refresh_virtualization_operations().await.unwrap();
    let pending = store.virtualization_operation(key).unwrap().unwrap();
    assert_eq!(pending.phase, VirtualizationOperationPhase::Running);
    assert_eq!(pending.task_id, original.task_id);
    assert!(pending.error.unwrap().contains("temporarily unavailable"));
    backend.task_state.store(1, Ordering::SeqCst);
    service.refresh_virtualization_operations().await.unwrap();
    let finished = store.virtualization_operation(key).unwrap().unwrap();
    assert_eq!(finished.phase, VirtualizationOperationPhase::Succeeded);
    assert!(finished.error.is_none());
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn interrupted_and_uncertain_submissions_hold_until_explicit_acknowledgement() {
    for interrupted in [false, true] {
        let (_directory, store) = setup();
        let backend = PowerBackend::new();
        backend.outcome.store(2, Ordering::SeqCst);
        let key = Uuid::new_v4();
        if interrupted {
            store
                .reserve_virtualization_operation(VirtualizationOperation {
                    id: key,
                    requested_by: "owner".into(),
                    guest: guest(),
                    action: VirtualizationPowerAction::Shutdown,
                    phase: VirtualizationOperationPhase::Submitting,
                    task_id: None,
                    error: None,
                    created_at: 1,
                    updated_at: 1,
                })
                .unwrap();
        }
        let service = Service::new_virtualization(store.clone(), backend.clone()).unwrap();
        if !interrupted {
            service
                .request(
                    "owner".into(),
                    request(key, VirtualizationPowerAction::Shutdown),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            dispatched(&store, key).await.phase,
            VirtualizationOperationPhase::Unknown
        );
        assert_eq!(
            backend.submissions.load(Ordering::SeqCst),
            usize::from(!interrupted)
        );
        assert!(
            service
                .request(
                    "owner".into(),
                    request(Uuid::new_v4(), VirtualizationPowerAction::Shutdown)
                )
                .await
                .is_err()
        );
        assert!(
            service
                .request(
                    "member".into(),
                    Request::AcknowledgeVirtualizationOperation { operation_id: key }
                )
                .await
                .is_err()
        );
        service
            .request(
                "owner".into(),
                Request::AcknowledgeVirtualizationOperation { operation_id: key },
            )
            .await
            .unwrap();
        service
            .request(
                "owner".into(),
                Request::AcknowledgeVirtualizationOperation { operation_id: key },
            )
            .await
            .unwrap();
        service
            .request(
                "owner".into(),
                request(key, VirtualizationPowerAction::Shutdown),
            )
            .await
            .unwrap();
        assert_eq!(
            store.virtualization_operation(key).unwrap().unwrap().phase,
            VirtualizationOperationPhase::Acknowledged
        );
        assert_eq!(
            backend.submissions.load(Ordering::SeqCst),
            usize::from(!interrupted)
        );
    }
}

#[tokio::test]
async fn rejection_and_task_failure_are_distinct_from_submission_uncertainty() {
    for rejection in [false, true] {
        let (_directory, store) = setup();
        let backend = PowerBackend::new();
        backend.outcome.store(u8::from(rejection), Ordering::SeqCst);
        backend.task_state.store(2, Ordering::SeqCst);
        let service = Service::new_virtualization(store.clone(), backend.clone()).unwrap();
        let key = Uuid::new_v4();
        service
            .request(
                "owner".into(),
                request(key, VirtualizationPowerAction::Shutdown),
            )
            .await
            .unwrap();
        dispatched(&store, key).await;
        service.refresh_virtualization_operations().await.unwrap();
        let operation = store.virtualization_operation(key).unwrap().unwrap();
        assert_eq!(operation.phase, VirtualizationOperationPhase::Failed);
        assert_eq!(operation.task_id.is_none(), rejection);
        assert_eq!(backend.submissions.load(Ordering::SeqCst), 1);
        assert!(
            service
                .request(
                    "owner".into(),
                    Request::AcknowledgeVirtualizationOperation { operation_id: key }
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn preflight_rejects_templates_missing_moved_stale_and_container_targets() {
    let (_directory, store) = setup();
    let backend = PowerBackend::new();
    let service = Service::new_virtualization(store.clone(), backend.clone()).unwrap();
    for identity in [
        VirtualizationGuestIdentity {
            vmid: 9000,
            ..guest()
        },
        VirtualizationGuestIdentity {
            node: "other-node".into(),
            ..guest()
        },
        guest(),
    ] {
        let key = Uuid::new_v4();
        service
            .request(
                "owner".into(),
                Request::VirtualizationPower {
                    idempotency_key: key,
                    guest: identity,
                    action: VirtualizationPowerAction::Start,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            dispatched(&store, key).await.phase,
            VirtualizationOperationPhase::Failed
        );
    }
    for identity in [
        VirtualizationGuestIdentity {
            kind: VirtualizationGuestKind::Container,
            ..guest()
        },
        VirtualizationGuestIdentity {
            node: "../node".into(),
            ..guest()
        },
        VirtualizationGuestIdentity { vmid: 0, ..guest() },
    ] {
        assert!(
            service
                .request(
                    "owner".into(),
                    Request::VirtualizationPower {
                        idempotency_key: Uuid::new_v4(),
                        guest: identity,
                        action: VirtualizationPowerAction::Shutdown
                    }
                )
                .await
                .is_err()
        );
    }
    assert_eq!(backend.submissions.load(Ordering::SeqCst), 0);
    store.revoke_device("owner", "owner").unwrap();
    assert!(
        service
            .request(
                "owner".into(),
                request(Uuid::new_v4(), VirtualizationPowerAction::Shutdown)
            )
            .await
            .is_err()
    );
}
