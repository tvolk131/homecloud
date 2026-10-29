use super::*;
use homecloud_host::Simulator;
use std::sync::atomic::{AtomicUsize, Ordering};

struct ApplyCounter {
    inner: Simulator,
    applies: AtomicUsize,
}

#[async_trait::async_trait]
impl Backend for ApplyCounter {
    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        if matches!(&request, HostRequest::Apply { .. }) {
            self.applies.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.request(request).await
    }
}

#[tokio::test]
async fn repeated_noop_actions_never_reach_the_host_during_a_guest_reboot() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::default();
    let store = Store::open(dir.path().join("state.db"), config.limits.clone()).unwrap();
    let (user, token) = store.bootstrap("Owner").unwrap();
    store.enroll(&token, "owner").unwrap();
    store
        .grant("owner", user.id, config.limits.capacity, true)
        .unwrap();
    let create = store.submit("owner", uuid::Uuid::new_v4(), Mutation::Create {
        spec: VmSpec { applications: vec![], name: "owned".into(), memory_gib: 4, cores: 1, disk_gib: 10,
            ssh_public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into() }
    }).unwrap();
    store
        .complete(create.id, "running".into(), None, false)
        .unwrap();
    store
        .observe(
            create.vm_id,
            "stopped".into(),
            Some("Guest reboot in progress".into()),
        )
        .unwrap();
    let before = serde_json::to_value(store.vm(create.vm_id).unwrap()).unwrap();
    let backend = Arc::new(ApplyCounter {
        inner: Simulator::new(dir.path().join("host.json")),
        applies: AtomicUsize::new(0),
    });
    let service = Service::new(store.clone(), backend.clone()).unwrap();
    let requests = [
        Mutation::Start {
            vm_id: create.vm_id,
            without_gpu: false,
        },
        Mutation::State {
            vm_id: create.vm_id,
            desired: Lifecycle::Running,
        },
        Mutation::Resize {
            vm_id: create.vm_id,
            memory_gib: 4,
            cores: 1,
            disk_gib: 10,
        },
        Mutation::Gpu {
            vm_id: create.vm_id,
            pci: None,
        },
    ];
    for mutation in requests.into_iter().cycle().take(24) {
        let Response::Operation(op) = service
            .request(
                "owner".into(),
                Request::Mutate {
                    mutation,
                    idempotency_key: uuid::Uuid::new_v4(),
                },
            )
            .await
            .unwrap()
        else {
            panic!("Expected operation")
        };
        assert_eq!(op.phase, "succeeded");
        service.reconcile_once().await.unwrap();
    }
    assert_eq!(backend.applies.load(Ordering::SeqCst), 0);
    assert_eq!(
        serde_json::to_value(store.vm(create.vm_id).unwrap()).unwrap(),
        before
    );
    // Positive control: a real size change still reaches the host exactly once.
    store.observe(create.vm_id, "running".into(), None).unwrap();
    service
        .request(
            "owner".into(),
            Request::Mutate {
                mutation: Mutation::Resize {
                    vm_id: create.vm_id,
                    memory_gib: 6,
                    cores: 1,
                    disk_gib: 10,
                },
                idempotency_key: uuid::Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    service.reconcile_once().await.unwrap();
    assert_eq!(backend.applies.load(Ordering::SeqCst), 1);
}
