use homecloud_client::{Client, DeviceProof, Profile, read_frame, write_frame};
use homecloud_core::*;
use homecloud_daemon::{Config, Service, serve};
use homecloud_host::Simulator;
use homecloud_store::Store;
use iroh::SecretKey;
use std::sync::Arc;

#[tokio::test]
async fn real_quic_requires_enrollment_and_rechecks_device_revocation() {
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
    let (client, user) = Client::enroll(&path, address.clone(), token.clone())
        .await
        .unwrap();
    assert_eq!(client.snapshot().await.unwrap().me.id, user.id);
    let sibling = Client::load(&path).await.unwrap();
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
    let (client, _) = Client::enroll(&path, address.clone(), token).await.unwrap();
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
    let profile = Profile::load(&path).unwrap();
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
    let client = Client::load(&path).await.unwrap();
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
