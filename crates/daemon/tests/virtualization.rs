use homecloud_core::*;
use homecloud_daemon::{Config, Service};
use homecloud_store::Store;
use homecloud_virtualization::{SimulatedVirtualization, VirtualizationBackend};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn standalone_and_legacy_state_directories_cannot_be_interchanged() {
    for first_virtualization in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        for (first, virtualization) in
            [(true, first_virtualization), (false, !first_virtualization)]
        {
            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_homecloudd"));
            command.arg("--state-dir").arg(directory.path());
            if virtualization {
                command.args(["--virtualization", "simulated"]);
            } else {
                command.arg("--simulate");
            }
            let output = command.arg("bootstrap-admin").output().unwrap();
            assert_eq!(
                output.status.success(),
                first,
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            if !first {
                assert!(String::from_utf8_lossy(&output.stderr).contains("state directory"));
            }
        }
        assert_eq!(
            directory.path().join("virtualization.db").exists(),
            first_virtualization
        );
        assert_eq!(
            directory.path().join("state.db").exists(),
            !first_virtualization
        );
    }
}

struct CountingBackend {
    inner: SimulatedVirtualization,
    calls: AtomicUsize,
}

#[tokio::test]
async fn authenticated_mode_discovery_distinguishes_controllers_without_host_calls() {
    assert!(!Request::ControlPlaneMode.changes_host_workload());
    for virtualization in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Store::open(directory.path().join("state.db"), Config::default().limits).unwrap();
        let (_, token) = store.bootstrap("Owner").unwrap();
        store.enroll(&token, "owner").unwrap();
        let service = if virtualization {
            Service::new_virtualization(store, Arc::new(SimulatedVirtualization::demo())).unwrap()
        } else {
            Service::new(
                store,
                Arc::new(homecloud_host::Simulator::new(
                    directory.path().join("host.json"),
                )),
            )
            .unwrap()
        };
        assert!(
            service
                .request("unknown".into(), Request::ControlPlaneMode)
                .await
                .is_err()
        );
        let Response::ControlPlaneMode(mode) = service
            .request("owner".into(), Request::ControlPlaneMode)
            .await
            .unwrap()
        else {
            panic!("Expected mode discovery response");
        };
        assert_eq!(
            mode,
            if virtualization {
                ControlPlaneMode::Virtualization
            } else {
                ControlPlaneMode::ManagedHost
            }
        );
        assert!(!directory.path().join("host.json").exists());
    }
}

#[async_trait::async_trait]
impl VirtualizationBackend for CountingBackend {
    async fn inventory(&self) -> anyhow::Result<VirtualizationInventory> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.inventory().await
    }
}

#[tokio::test]
async fn inventory_is_admin_only_and_does_not_use_tenant_resource_grants() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("state.db"), Config::default().limits).unwrap();
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
    let backend = Arc::new(CountingBackend {
        inner: SimulatedVirtualization::demo(),
        calls: AtomicUsize::new(0),
    });
    let service = Service::new_virtualization(store.clone(), backend.clone()).unwrap();
    for peer in ["unknown", "member"] {
        assert!(
            service
                .request(peer.into(), Request::ControlPlaneMode)
                .await
                .is_err()
        );
        assert!(
            service
                .request(peer.into(), Request::VirtualizationInventory)
                .await
                .is_err()
        );
    }
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    let Response::VirtualizationInventory(inventory) = service
        .request("owner".into(), Request::VirtualizationInventory)
        .await
        .unwrap()
    else {
        panic!("Expected virtualization inventory");
    };
    assert_eq!(inventory.source, VirtualizationSource::Simulated);
    assert_eq!(inventory.guests[0].vmid, 101);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert!(store.vms().unwrap().is_empty());
    let owner = store.authenticate("owner").unwrap();
    assert_eq!(owner.grant, Resources::default());
    assert_eq!(owner.committed, Resources::default());
}

#[tokio::test]
async fn owner_only_mode_rejects_legacy_control_plane_operations() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("state.db"), Config::default().limits).unwrap();
    let (owner, token) = store.bootstrap("Owner").unwrap();
    store.enroll(&token, "owner").unwrap();
    let service =
        Service::new_virtualization(store.clone(), Arc::new(SimulatedVirtualization::demo()))
            .unwrap();
    for request in [
        Request::Snapshot,
        Request::Grant {
            user_id: owner.id,
            grant: Resources {
                memory_bytes: GIB,
                cpu_cores: 1,
                disk_bytes: GIB,
            },
            gpu_allowed: false,
        },
        Request::Invite {
            name: "Another".into(),
            role: Role::Admin,
            grant: Resources::default(),
            gpu_allowed: false,
            expires_hours: 1,
        },
        Request::Mutate {
            idempotency_key: uuid::Uuid::new_v4(),
            mutation: Mutation::State {
                vm_id: uuid::Uuid::new_v4(),
                desired: Lifecycle::Running,
            },
        },
    ] {
        let error = service.request("owner".into(), request).await.unwrap_err();
        assert!(error.to_string().contains("Legacy control-plane"));
    }
    assert!(store.vms().unwrap().is_empty());
    assert_eq!(
        store.authenticate("owner").unwrap().grant,
        Resources::default()
    );
    assert!(
        service
            .backend
            .request(homecloud_host::HostRequest::Inventory)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn device_revocation_is_effective_on_the_next_inventory_request() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("state.db"), Config::default().limits).unwrap();
    let (owner, token) = store.bootstrap("Owner").unwrap();
    store.enroll(&token, "owner").unwrap();
    let (token, _) = store.invite_device("owner", owner.id, 1).unwrap();
    store.enroll(&token, "laptop").unwrap();
    let backend = Arc::new(CountingBackend {
        inner: SimulatedVirtualization::demo(),
        calls: AtomicUsize::new(0),
    });
    let service = Service::new_virtualization(store, backend.clone()).unwrap();
    service
        .request("laptop".into(), Request::VirtualizationInventory)
        .await
        .unwrap();
    service
        .request(
            "owner".into(),
            Request::RevokeDevice {
                peer: "laptop".into(),
            },
        )
        .await
        .unwrap();
    assert!(
        service
            .request("laptop".into(), Request::VirtualizationInventory)
            .await
            .is_err()
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}
