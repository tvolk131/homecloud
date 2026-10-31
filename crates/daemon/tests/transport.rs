use homecloud_client::{Client, DeviceProof, Profile, read_frame, write_frame};
use homecloud_core::*;
use homecloud_daemon::{Config, Service, serve};
use homecloud_host::{Backend, HostRequest, HostResponse, Simulator};
use homecloud_store::Store;
use iroh::SecretKey;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[tokio::test]
async fn real_quic_storage_preserves_nested_observations_and_redacts_members() {
    struct StorageBackend {
        inner: Simulator,
        inventory: StorageInventory,
        calls: AtomicUsize,
        failed: AtomicBool,
    }
    #[async_trait::async_trait]
    impl Backend for StorageBackend {
        async fn request(&self, request: HostRequest) -> anyhow::Result<HostResponse> {
            if matches!(request, HostRequest::Storage) {
                self.calls.fetch_add(1, Ordering::SeqCst);
                anyhow::ensure!(
                    !self.failed.load(Ordering::SeqCst),
                    "Injected storage collection failure"
                );
                return Ok(HostResponse::Storage(self.inventory.clone()));
            }
            self.inner.request(request).await
        }
    }

    let credentials = TestCredentials::default();
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("state.db"), Config::default().limits).unwrap();
    let (_, token) = store.bootstrap("Administrator").unwrap();
    let expected = storage_inventory();
    let backend = Arc::new(StorageBackend {
        inner: Simulator::new(temp.path().join("observed.json")),
        inventory: expected.clone(),
        calls: AtomicUsize::new(0),
        failed: AtomicBool::new(false),
    });
    let service = Service::new(store, backend.clone()).unwrap();
    let host = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![CONTROL_ALPN.to_vec()])
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let address = iroh::EndpointAddr::new(host.id()).with_ip_addr(
        host.bound_sockets()
            .into_iter()
            .find(|socket| socket.ip().is_loopback())
            .unwrap(),
    );
    let task = tokio::spawn(serve(service.clone(), host.clone()));
    let (admin, admin_user) = Client::enroll_with_store(
        &temp.path().join("admin.json"),
        address.clone(),
        token,
        Default::default(),
        &credentials,
    )
    .await
    .unwrap();
    let (token, member_id, _) = service
        .store
        .invite(
            &admin.device_id(),
            "Member",
            Role::User,
            Resources::default(),
            false,
            1,
        )
        .unwrap();
    let (member, _) = Client::enroll_with_store(
        &temp.path().join("member.json"),
        address,
        token,
        Default::default(),
        &credentials,
    )
    .await
    .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    service.collect_storage_once().await.unwrap();
    for _ in 0..2 {
        let admin_response = admin.rpc_response(Request::Snapshot).await.unwrap();
        assert!(serde_json::to_vec(&admin_response).unwrap().len() < MAX_FRAME);
        let Response::Snapshot(snapshot) = admin_response else {
            panic!("Expected administrator snapshot");
        };
        assert_eq!(snapshot.me.id, admin_user.id);
        assert_eq!(snapshot.me.role, Role::Admin);
        assert_eq!(
            serde_json::to_value(snapshot.inventory.storage.as_ref().unwrap()).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );

        let member_response = member.rpc_response(Request::Snapshot).await.unwrap();
        let encoded = serde_json::to_value(&member_response).unwrap();
        assert!(encoded["data"]["inventory"].get("storage").is_none());
        let encoded = encoded.to_string();
        for marker in [
            "SYNTHETIC-PRIVATE-SERIAL",
            "SYNTHETIC-PRIVATE-POOL",
            "/dev/sda",
            "/dev/nvme0n1",
            "18446744073709551616",
        ] {
            assert!(!encoded.contains(marker), "Member response leaked {marker}");
        }
        let Response::Snapshot(snapshot) = member_response else {
            panic!("Expected member snapshot");
        };
        assert_eq!(snapshot.me.id, member_id);
        assert_eq!(snapshot.me.role, Role::User);
        assert!(snapshot.inventory.storage.is_none());
    }
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);

    backend.failed.store(true, Ordering::SeqCst);
    assert!(service.collect_storage_once().await.is_err());
    let snapshot = admin.snapshot().await.unwrap();
    let storage = snapshot.inventory.storage.unwrap();
    assert_eq!(
        serde_json::to_value(&storage.disks).unwrap(),
        serde_json::to_value(&expected.disks).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&storage.pools).unwrap(),
        serde_json::to_value(&expected.pools).unwrap()
    );
    for (collection, previous) in [
        (&storage.disk_collection, &expected.disk_collection),
        (&storage.pool_collection, &expected.pool_collection),
    ] {
        assert_eq!(collection.observed_at, previous.observed_at);
        assert!(collection.attempted_at.unwrap() >= previous.attempted_at.unwrap());
        assert_eq!(
            collection.error.as_deref(),
            Some("Injected storage collection failure")
        );
    }
    assert!(member.snapshot().await.unwrap().inventory.storage.is_none());
    assert_eq!(backend.calls.load(Ordering::SeqCst), 2);

    admin.close().await;
    member.close().await;
    host.close().await;
    task.abort();
}

#[tokio::test]
async fn storage_frames_enforce_the_limit_before_writing_or_reading_the_body() {
    let mut inventory = storage_inventory();
    inventory.warnings.push(String::new());
    let length = serde_json::to_vec(&inventory).unwrap().len();
    *inventory.warnings.last_mut().unwrap() = "x".repeat(MAX_FRAME - length);
    assert_eq!(serde_json::to_vec(&inventory).unwrap().len(), MAX_FRAME);

    let mut frame = Vec::new();
    write_frame(&mut frame, &inventory).await.unwrap();
    assert_eq!(frame.len(), MAX_FRAME + 4);
    let decoded: StorageInventory = read_frame(&mut frame.as_slice()).await.unwrap();
    assert_eq!(
        serde_json::to_value(&decoded).unwrap(),
        serde_json::to_value(&inventory).unwrap()
    );

    inventory.warnings.last_mut().unwrap().push('x');
    let mut rejected = Vec::new();
    let error = write_frame(&mut rejected, &inventory).await.unwrap_err();
    assert_eq!(error.to_string(), "Frame too large");
    assert!(rejected.is_empty());

    let oversized_length = ((MAX_FRAME + 1) as u32).to_be_bytes();
    let error = read_frame::<_, StorageInventory>(&mut oversized_length.as_slice())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "Frame too large");
}

fn storage_inventory() -> StorageInventory {
    let collection = StorageCollection {
        attempted_at: Some(1_790_000_000),
        observed_at: Some(1_790_000_000),
        error: None,
    };
    let disks = vec![
        PhysicalDisk {
            id: "wwn:0x5000c50000000001".into(),
            path: "/dev/sda".into(),
            model: Some("Synthetic SATA".into()),
            serial: Some("SYNTHETIC-PRIVATE-SERIAL-SATA".into()),
            wwn: Some("0x5000c50000000001".into()),
            transport: Some("sata".into()),
            size_bytes: Some(2_000_000_000_000),
            rotational: Some(true),
            devices: vec![
                StorageBlockDevice {
                    path: "/dev/sda1".into(),
                    filesystem: Some("vfat".into()),
                    mountpoints: vec!["/boot/efi".into()],
                },
                StorageBlockDevice {
                    path: "/dev/sda2".into(),
                    filesystem: Some("zfs_member".into()),
                    mountpoints: vec![],
                },
            ],
            smart: SmartObservation {
                collection: collection.clone(),
                state: SmartState::Current,
                passed: Some(true),
                temperature_celsius: Some(32),
                power_on_hours: Some(12_345),
                metrics: vec![SmartMetric {
                    name: "reallocated_sector_ct".into(),
                    value: "0".into(),
                }],
                self_tests: vec!["Short offline: Completed without error".into()],
            },
        },
        PhysicalDisk {
            id: "wwn:eui.0000000000000002".into(),
            path: "/dev/nvme0n1".into(),
            model: Some("Synthetic NVMe".into()),
            serial: Some("SYNTHETIC-PRIVATE-SERIAL-NVME".into()),
            wwn: Some("eui.0000000000000002".into()),
            transport: Some("nvme".into()),
            size_bytes: Some(2_000_000_000_000),
            rotational: Some(false),
            devices: vec![StorageBlockDevice {
                path: "/dev/nvme0n1p1".into(),
                filesystem: Some("zfs_member".into()),
                mountpoints: vec![],
            }],
            smart: SmartObservation {
                collection: StorageCollection {
                    attempted_at: Some(1_789_999_900),
                    observed_at: Some(1_789_999_900),
                    error: None,
                },
                state: SmartState::Current,
                passed: Some(true),
                temperature_celsius: Some(39),
                power_on_hours: Some(4_321),
                metrics: vec![SmartMetric {
                    name: "data_units_read".into(),
                    value: "18446744073709551616".into(),
                }],
                self_tests: vec![],
            },
        },
    ];
    let children = disks
        .iter()
        .enumerate()
        .map(|(index, disk)| StorageVdev {
            guid: format!("1844674407370955161{index}"),
            name: disk.devices.last().unwrap().path.clone(),
            kind: "disk".into(),
            role: "data".into(),
            state: "ONLINE".into(),
            path: Some(disk.devices.last().unwrap().path.clone()),
            disk_id: Some(disk.id.clone()),
            read_errors: Some(0),
            write_errors: Some(0),
            checksum_errors: Some(index as u64),
            children: vec![],
        })
        .collect();
    StorageInventory {
        disk_collection: collection.clone(),
        pool_collection: collection,
        disks,
        pools: vec![StoragePool {
            name: "SYNTHETIC-PRIVATE-POOL".into(),
            guid: "18446744073709551600".into(),
            state: "ONLINE".into(),
            managed: true,
            status: Some("No known data errors".into()),
            scan: Some("scrub repaired 0B with 0 errors".into()),
            vdevs: vec![StorageVdev {
                guid: "18446744073709551601".into(),
                name: "mirror-0".into(),
                kind: "mirror".into(),
                role: "data".into(),
                state: "ONLINE".into(),
                read_errors: Some(0),
                write_errors: Some(0),
                checksum_errors: Some(1),
                children,
                ..Default::default()
            }],
            warnings: vec!["Synthetic pool warning".into()],
        }],
        warnings: vec!["Synthetic storage warning".into()],
    }
}

#[tokio::test]
async fn real_quic_requires_enrollment_and_rechecks_device_revocation() {
    let credentials = TestCredentials::default();
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("state.db"), Config::default().limits).unwrap();
    let (_, token) = store.bootstrap("Administrator").unwrap();
    let service = Service::new(
        store,
        Arc::new(Simulator::new(temp.path().join("observed.json"))),
    )
    .unwrap();
    let host = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(SecretKey::generate())
        .alpns(vec![CONTROL_ALPN.to_vec(), SSH_ALPN.to_vec()])
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let socket = host
        .bound_sockets()
        .into_iter()
        .find(|s| s.ip().is_loopback())
        .unwrap();
    let address = iroh::EndpointAddr::new(host.id()).with_ip_addr(socket);
    let task = tokio::spawn(serve(service.clone(), host.clone()));
    let stranger = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let connection = stranger.connect(address.clone(), SSH_ALPN).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(
        &mut send,
        &DeviceProof::sign(&connection, &SecretKey::generate()).unwrap(),
    )
    .await
    .unwrap();
    homecloud_client::write_frame(
        &mut send,
        &Request::Ssh {
            vm_id: uuid::Uuid::new_v4(),
        },
    )
    .await
    .unwrap();
    let response: Response = homecloud_client::read_frame(&mut recv).await.unwrap();
    assert!(matches!(response,Response::Error{message} if message.contains("authorized")));
    connection.close(0u32.into(), b"denial received");
    stranger.close().await;
    let path = temp.path().join("client.json");
    let outsider = Client::from_profile(Profile {
        server: address.clone(),
        device_secret: hex_key(),
    })
    .await
    .unwrap();
    assert!(outsider.snapshot().await.is_err());
    let (client, user) = Client::enroll_with_store(
        &path,
        address.clone(),
        token.clone(),
        Default::default(),
        &credentials,
    )
    .await
    .unwrap();
    assert_eq!(client.snapshot().await.unwrap().me.id, user.id);
    let sibling = Client::from_profile(Profile::load_with_store(&path, &credentials).unwrap())
        .await
        .unwrap();
    assert_eq!(client.device_id(), sibling.device_id());
    assert_ne!(client.transport_id(), sibling.transport_id());
    let (first, second) = tokio::join!(client.snapshot(), sibling.snapshot());
    assert_eq!(first.unwrap().me.id, user.id);
    assert_eq!(second.unwrap().me.id, user.id);
    assert!(outsider.rpc(Request::Enroll { token }).await.is_err());
    client
        .rpc(Request::RevokeDevice {
            peer: client.device_id(),
        })
        .await
        .unwrap();
    assert!(client.snapshot().await.is_err());
    assert!(sibling.snapshot().await.is_err());
    // A persisted revoked device cannot regain enrollment with another token.
    let token = service.store.recover_admin(user.id).unwrap();
    assert!(client.rpc(Request::Enroll { token }).await.is_err());
    host.close().await;
    task.abort();
}
fn hex_key() -> String {
    SecretKey::generate()
        .to_bytes()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}

#[tokio::test]
async fn device_proof_cannot_be_forged_or_replayed_on_another_tls_connection() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("state.db"), Config::default().limits).unwrap();
    let (_, token) = store.bootstrap("Administrator").unwrap();
    let device = SecretKey::generate();
    store.enroll(&token, &device.public().to_string()).unwrap();
    let service = Service::new(
        store,
        Arc::new(Simulator::new(temp.path().join("observed.json"))),
    )
    .unwrap();
    let host = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![CONTROL_ALPN.to_vec(), SSH_ALPN.to_vec()])
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let socket = host
        .bound_sockets()
        .into_iter()
        .find(|s| s.ip().is_loopback())
        .unwrap();
    let address = iroh::EndpointAddr::new(host.id()).with_ip_addr(socket);
    let task = tokio::spawn(serve(service, host.clone()));
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let first = endpoint
        .connect(address.clone(), CONTROL_ALPN)
        .await
        .unwrap();
    let proof = DeviceProof::sign(&first, &device).unwrap();
    let (mut send, mut recv) = first.open_bi().await.unwrap();
    write_frame(&mut send, &proof).await.unwrap();
    write_frame(&mut send, &Request::Snapshot).await.unwrap();
    send.finish().unwrap();
    assert!(matches!(
        read_frame::<_, Response>(&mut recv).await.unwrap(),
        Response::Snapshot(_)
    ));
    first.close(0u32.into(), b"complete");

    for forged in [false, true] {
        let connection = endpoint
            .connect(address.clone(), CONTROL_ALPN)
            .await
            .unwrap();
        let invalid = if forged {
            let mut proof = DeviceProof::sign(&connection, &SecretKey::generate()).unwrap();
            proof.device = device.public();
            proof
        } else {
            proof.clone()
        };
        assert!(invalid.verify(&connection).is_err());
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        write_frame(&mut send, &invalid).await.unwrap();
        write_frame(&mut send, &Request::Snapshot).await.unwrap();
        send.finish().unwrap();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            read_frame::<_, Response>(&mut recv),
        )
        .await
        .unwrap();
        assert!(
            response.is_err(),
            "invalid device proof must fail before dispatch"
        );
    }
    endpoint.close().await;
    host.close().await;
    task.abort();
}

#[tokio::test]
async fn slow_control_reader_receives_the_complete_response_before_connection_release() {
    struct LargeInventory;
    #[async_trait::async_trait]
    impl homecloud_host::Backend for LargeInventory {
        async fn request(
            &self,
            _: homecloud_host::HostRequest,
        ) -> anyhow::Result<homecloud_host::HostResponse> {
            Ok(homecloud_host::HostResponse::Inventory(HostInventory {
                warnings: vec!["slow reader fixture".repeat(24000)],
                ..Default::default()
            }))
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("state.db"), Config::default().limits).unwrap();
    let (_, token) = store.bootstrap("Administrator").unwrap();
    let device = SecretKey::generate();
    store.enroll(&token, &device.public().to_string()).unwrap();
    let service = Service::new(store, Arc::new(LargeInventory)).unwrap();
    let host = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![CONTROL_ALPN.to_vec(), SSH_ALPN.to_vec()])
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let address = iroh::EndpointAddr::new(host.id()).with_ip_addr(
        host.bound_sockets()
            .into_iter()
            .find(|s| s.ip().is_loopback())
            .unwrap(),
    );
    let task = tokio::spawn(serve(service, host.clone()));
    let transport = iroh::endpoint::QuicTransportConfig::builder()
        .stream_receive_window(1024u32.into())
        .build();
    let client = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .transport_config(transport)
        .bind()
        .await
        .unwrap();
    let connection = client.connect(address, CONTROL_ALPN).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    write_frame(&mut send, &DeviceProof::sign(&connection, &device).unwrap())
        .await
        .unwrap();
    write_frame(&mut send, &Request::Snapshot).await.unwrap();
    send.finish().unwrap();
    // Keep the response flow-controlled beyond the old fixed five-second grace.
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_frame::<_, Response>(&mut recv),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(response, Response::Snapshot(ref s) if s.inventory.warnings[0].len() > 32*1024)
    );
    connection.close(0u32.into(), b"complete");
    client.close().await;
    host.close().await;
    task.abort();
}

#[tokio::test]
async fn recoverable_create_survives_an_unread_quic_response_and_fences_rejection() {
    use std::time::{Duration, Instant};
    let credentials = TestCredentials::default();
    let temp = tempfile::tempdir().unwrap();
    let limits = Config::default().limits;
    let store = Store::open(temp.path().join("state.db"), limits.clone()).unwrap();
    let (user, token) = store.bootstrap("Owner").unwrap();
    let grant = Resources {
        memory_bytes: 2 * GIB,
        cpu_cores: 1,
        disk_bytes: 12 * GIB,
    };
    let service = Service::new(
        store,
        Arc::new(Simulator::new(temp.path().join("host.json"))),
    )
    .unwrap();
    let host = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(SecretKey::generate())
        .alpns(vec![CONTROL_ALPN.to_vec()])
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let socket = host
        .bound_sockets()
        .into_iter()
        .find(|s| s.ip().is_loopback())
        .unwrap();
    let address = iroh::EndpointAddr::new(host.id()).with_ip_addr(socket);
    let worker = tokio::spawn(serve(service.clone(), host.clone()));
    let path = temp.path().join("client.json");
    let (client, _) = Client::enroll_with_store(
        &path,
        address.clone(),
        token,
        Default::default(),
        &credentials,
    )
    .await
    .unwrap();
    service
        .store
        .grant(&client.device_id(), user.id, grant, false)
        .unwrap();
    let spec = VmSpec {
        name: "lost-response".into(),
        memory_gib: 2,
        cores: 1,
        disk_gib: 10,
        applications: vec![],
        ssh_public_key:
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                .into(),
    };
    let key = uuid::Uuid::new_v4();
    let request = Request::CreateVm {
        idempotency_key: key,
        spec: spec.clone(),
    };
    let profile = Profile::load_with_store(&path, &credentials).unwrap();
    let bytes: [u8; 32] = hex_decode(&profile.device_secret);
    let device = SecretKey::from_bytes(&bytes);
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let conn = endpoint.connect(address, CONTROL_ALPN).await.unwrap();
    let (mut send, _recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &DeviceProof::sign(&conn, &device).unwrap())
        .await
        .unwrap();
    write_frame(&mut send, &request).await.unwrap();
    send.finish().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while service.store.vms().unwrap().is_empty() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    conn.close(0u32.into(), b"simulate lost reply without reading it");
    endpoint.close().await;
    client.close().await;
    let client = Client::from_profile(Profile::load_with_store(&path, &credentials).unwrap())
        .await
        .unwrap();
    let Response::Operation(first) = client.rpc_response(request.clone()).await.unwrap() else {
        panic!("Missing original operation")
    };
    let Response::Operation(second) = client.rpc_response(request).await.unwrap() else {
        panic!("Replay failed")
    };
    assert_eq!((first.id, first.vm_id), (second.id, second.vm_id));
    assert_eq!(service.store.vms().unwrap().len(), 1);
    assert_eq!(service.store.users().unwrap()[0].committed, grant);
    let rejected_key = uuid::Uuid::new_v4();
    let rejected = Request::CreateVm {
        idempotency_key: rejected_key,
        spec,
    };
    assert!(
        matches!(client.rpc_response(rejected.clone()).await.unwrap(),Response::CreateRejected { idempotency_key,.. } if idempotency_key==rejected_key)
    );
    service
        .store
        .grant(&client.device_id(), user.id, limits.capacity, false)
        .unwrap();
    assert!(matches!(
        client.rpc_response(rejected).await.unwrap(),
        Response::CreateRejected { .. }
    ));
    service
        .store
        .revoke_device(&client.device_id(), &client.device_id())
        .unwrap();
    assert!(matches!(
        client.rpc_response(Request::Snapshot).await.unwrap(),
        Response::Error { .. }
    ));
    assert!(client.snapshot().await.is_err());
    assert_eq!(service.store.vms().unwrap().len(), 1);
    client.close().await;
    host.close().await;
    worker.abort();
}
fn hex_decode(value: &str) -> [u8; 32] {
    let mut bytes = [0; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&value[2 * i..2 * i + 2], 16).unwrap();
    }
    bytes
}

#[derive(Default)]
struct TestCredentials(std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>);
impl homecloud_client::CredentialStore for TestCredentials {
    fn get(&self, account: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn put(&self, account: &str, secret: &[u8]) -> anyhow::Result<()> {
        self.0.lock().unwrap().insert(account.into(), secret.into());
        Ok(())
    }
}
