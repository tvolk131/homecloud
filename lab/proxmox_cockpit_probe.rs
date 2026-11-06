use anyhow::{Result, ensure};
use homecloud_client::{Client, Profile};
use homecloud_core::*;
use homecloud_daemon::{Config, Service};
use homecloud_store::Store;
use homecloud_virtualization::{ProxmoxConnection, connect_proxmox};
use serde_json::{Value, json};

async fn validate(mode: &str, filename: &str) -> Result<Value> {
    let config: ProxmoxConnection = serde_json::from_slice(&std::fs::read(filename)?)?;
    let backend = connect_proxmox(config)?;
    if mode == "direct" {
        return Ok(json!({"inventory": backend.inventory().await?}));
    }
    ensure!(mode == "iroh", "Unknown probe mode");
    let parent = std::path::Path::new(filename).parent().ok_or_else(|| anyhow::anyhow!("Missing private config directory"))?;
    let directory = tempfile::Builder::new().prefix("iroh-probe-").tempdir_in(parent)?;
    let store = Store::open(directory.path().join("virtualization.db"), Config::default().limits)?;
    let (_, invitation) = store.bootstrap("Live validation owner")?;
    let service = Service::new_virtualization(store.clone(), backend)?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![CONTROL_ALPN.to_vec()])
        .bind_addr("127.0.0.1:0")?
        .bind()
        .await?;
    let address = iroh::EndpointAddr::new(endpoint.id()).with_ip_addr(
        endpoint.bound_sockets().into_iter().find(|socket| socket.ip().is_loopback()).unwrap(),
    );
    let server = tokio::spawn(homecloud_daemon::serve(service.clone(), endpoint.clone()));
    let outcome = async {
        let (owner, _) = Client::enroll(
            &directory.path().join("owner.json"), address.clone(), invitation,
        ).await?;
        ensure!(owner.control_plane_mode().await? == ControlPlaneMode::Virtualization, "Wrong control-plane mode");
        let inventory = owner.virtualization_inventory().await?;
        let legacy_denied = owner.rpc(Request::Snapshot).await.is_err();
        let (invitation, _, _) = store.invite(
            &owner.device_id(), "Live validation member", Role::User, Resources::default(), false, 1,
        )?;
        let (member, _) = Client::enroll(
            &directory.path().join("member.json"), address.clone(), invitation,
        ).await?;
        let member_denied = member.virtualization_inventory().await.is_err();
        let unknown = Client::from_profile(Profile {
            server: address,
            device_secret: hex::encode(iroh::SecretKey::generate().to_bytes()),
        }).await?;
        let unknown_denied = unknown.virtualization_inventory().await.is_err();
        store.revoke_device(&owner.device_id(), &owner.device_id())?;
        let revoked_denied = owner.virtualization_inventory().await.is_err();
        owner.close().await;
        member.close().await;
        unknown.close().await;
        ensure!(legacy_denied && member_denied && unknown_denied && revoked_denied, "Authorization regression");
        Ok(json!({
            "inventory": inventory, "authenticated_iroh": true, "transport": "loopback QUIC",
            "legacy_denied": legacy_denied, "member_denied": member_denied,
            "unknown_denied": unknown_denied, "revoked_denied": revoked_denied,
            "legacy_vms_created": store.vms()?.len(),
        }))
    }.await;
    endpoint.close().await;
    server.abort();
    outcome
}

#[tokio::main]
async fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let outcome = if arguments.len() == 3 {
        validate(&arguments[1], &arguments[2]).await
    } else {
        Err(anyhow::anyhow!("Expected mode and private config filename"))
    };
    match outcome {
        Ok(value) => println!("{}", json!({"ok": true, "result": value})),
        Err(error) => println!("{}", json!({"ok": false, "error": format!("{error:#}")})),
    }
}
