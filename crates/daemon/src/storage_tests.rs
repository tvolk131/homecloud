use super::*;
use homecloud_host::Simulator;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Notify;

struct StorageBackend {
    inner: Simulator,
    calls: AtomicUsize,
    failed: AtomicBool,
    blocked: AtomicBool,
    entered: Notify,
    release: Notify,
}

#[async_trait::async_trait]
impl Backend for StorageBackend {
    async fn request(&self, request: HostRequest) -> Result<HostResponse> {
        if matches!(request, HostRequest::Storage) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.blocked.load(Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            ensure!(
                !self.failed.load(Ordering::SeqCst),
                "Injected storage failure"
            );
            return Ok(HostResponse::Storage(StorageInventory {
                disk_collection: StorageCollection {
                    observed_at: Some(100),
                    ..Default::default()
                },
                disks: vec![PhysicalDisk {
                    serial: Some("SYNTHETIC-PRIVATE-SERIAL".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }));
        }
        self.inner.request(request).await
    }
}

fn fixture() -> (tempfile::TempDir, Arc<Service>, Arc<StorageBackend>) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("state.db"), Config::default().limits).unwrap();
    let (_, token) = store.bootstrap("Administrator").unwrap();
    store.enroll(&token, "admin").unwrap();
    let (token, _, _) = store
        .invite(
            "admin",
            "Member",
            Role::User,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    store.enroll(&token, "member").unwrap();
    let backend = Arc::new(StorageBackend {
        inner: Simulator::new(directory.path().join("host.json")),
        calls: AtomicUsize::new(0),
        failed: AtomicBool::new(false),
        blocked: AtomicBool::new(false),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let service = Service::new(store, backend.clone()).unwrap();
    (directory, service, backend)
}

#[tokio::test]
async fn storage_is_cached_admin_only_and_never_collected_by_snapshot() {
    let (_directory, service, backend) = fixture();
    service.collect_storage_once().await.unwrap();
    for peer in ["admin", "admin", "member"] {
        let Response::Snapshot(snapshot) = service
            .request(peer.into(), Request::Snapshot)
            .await
            .unwrap()
        else {
            panic!("Expected snapshot")
        };
        if peer == "admin" {
            assert_eq!(
                snapshot.inventory.storage.as_ref().unwrap().disks[0]
                    .serial
                    .as_deref(),
                Some("SYNTHETIC-PRIVATE-SERIAL")
            );
        } else {
            assert!(snapshot.inventory.storage.is_none());
            assert!(
                !serde_json::to_string(&snapshot)
                    .unwrap()
                    .contains("SYNTHETIC-PRIVATE-SERIAL")
            );
        }
    }
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn storage_failure_keeps_previous_readings_and_marks_both_sources_unavailable() {
    let (_directory, service, backend) = fixture();
    service.collect_storage_once().await.unwrap();
    backend.failed.store(true, Ordering::SeqCst);
    assert!(service.collect_storage_once().await.is_err());
    let inventory = service.storage.read().await;
    assert_eq!(inventory.disk_collection.observed_at, Some(100));
    assert_eq!(inventory.disks.len(), 1);
    for collection in [&inventory.disk_collection, &inventory.pool_collection] {
        assert!(collection.attempted_at.is_some());
        assert!(
            collection
                .error
                .as_ref()
                .unwrap()
                .contains("Injected storage failure")
        );
    }
    drop(inventory);
    backend.failed.store(false, Ordering::SeqCst);
    service.collect_storage_once().await.unwrap();
    let inventory = service.storage.read().await;
    assert!(inventory.disk_collection.error.is_none());
    assert!(inventory.pool_collection.error.is_none());
}

#[tokio::test]
async fn slow_storage_probe_does_not_block_snapshot_readers() {
    let (_directory, service, backend) = fixture();
    backend.blocked.store(true, Ordering::SeqCst);
    let worker = service.clone();
    let task = tokio::spawn(async move { worker.collect_storage_once().await });
    backend.entered.notified().await;
    let snapshot = tokio::time::timeout(
        Duration::from_secs(2),
        service.request("admin".into(), Request::Snapshot),
    )
    .await;
    backend.release.notify_one();
    task.await.unwrap().unwrap();
    assert!(matches!(snapshot.unwrap().unwrap(), Response::Snapshot(_)));
}
