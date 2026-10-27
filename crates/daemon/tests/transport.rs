use homecloud_client::{Client, Profile};
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
    );
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
    assert!(outsider.rpc(Request::Enroll { token }).await.is_err());
    client
        .rpc(Request::RevokeDevice {
            peer: client.device_id(),
        })
        .await
        .unwrap();
    assert!(client.snapshot().await.is_err());
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
