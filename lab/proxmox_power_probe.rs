use anyhow::{Result, ensure};
use homecloud_client::{Client, Profile};
use homecloud_core::*;
use homecloud_daemon::{Config, Service};
use homecloud_store::Store;
use homecloud_virtualization::{ProxmoxConnection, connect_proxmox};
use serde_json::{Value, json};
use std::{path::Path, time::{Duration, Instant}};

async fn record(store: &Store, identifier: &str) -> Result<VirtualizationOperation> {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let operation = store.virtualization_operation(identifier.parse()?)?
            .ok_or_else(|| anyhow::anyhow!("Missing durable power operation"))?;
        if operation.phase != VirtualizationOperationPhase::Submitting {
            return Ok(operation);
        }
        ensure!(Instant::now() < deadline, "Submission did not settle");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn complete(owner: &Client, identifier: &str) -> Result<VirtualizationOperation> {
    let deadline = Instant::now() + Duration::from_secs(100);
    loop {
        let operation = owner.virtualization_operations().await?.into_iter()
            .find(|operation| operation.id.to_string() == identifier)
            .ok_or_else(|| anyhow::anyhow!("Missing tracked operation"))?;
        if !operation.phase.blocks_guest() {
            return Ok(operation);
        }
        ensure!(operation.phase != VirtualizationOperationPhase::Unknown, "Unexpected ambiguous submission: {:?}", operation.error);
        ensure!(Instant::now() < deadline, "Power task did not complete: {:?}", operation.error);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn submit(owner: &Client, store: &Store, guest: &VirtualizationGuestIdentity,
                identifier: &str, action: VirtualizationPowerAction) -> Result<Value> {
    let count_before = store.virtualization_operations()?.len();
    let initial = owner.virtualization_power(identifier.parse()?, guest.clone(), action).await?;
    ensure!(initial.phase == VirtualizationOperationPhase::Submitting && initial.task_id.is_none(), "Submit did not return durable Submitting");
    let duplicate = owner.virtualization_power(identifier.parse()?, guest.clone(), action).await?;
    ensure!(duplicate.id == initial.id, "Idempotency key created another operation");
    ensure!(store.virtualization_operations()?.len() == count_before + 1, "Duplicate operation record created");
    let tracked = record(store, identifier).await?;
    ensure!(tracked.phase == VirtualizationOperationPhase::Running && tracked.task_id.is_some(), "Task not accepted: {:?}", tracked);
    ensure!(owner.acknowledge_virtualization_operation(tracked.id).await.is_err(), "Running operation was acknowledged");
    if action == VirtualizationPowerAction::Shutdown {
        return Ok(json!({"initial": initial, "duplicate": duplicate, "running": tracked,
                         "operation_count_before": count_before, "operation_count_after": count_before + 1}));
    }
    let terminal = complete(owner, identifier).await?;
    ensure!(terminal.phase == VirtualizationOperationPhase::Succeeded, "Power task failed: {:?}", terminal);
    let replay = owner.virtualization_power(identifier.parse()?, guest.clone(), action).await?;
    ensure!(replay == terminal, "Completed idempotency replay changed operation");
    ensure!(store.virtualization_operations()?.len() == count_before + 1, "Completed replay added an operation");
    Ok(json!({"initial": initial, "duplicate": duplicate, "running": tracked, "terminal": terminal, "replay": replay,
              "operation_count_before": count_before, "operation_count_after": count_before + 1}))
}

async fn validate(mode: &str, filename: &str) -> Result<Value> {
    let path = Path::new(filename).parent().ok_or_else(|| anyhow::anyhow!("No private probe directory"))?;
    let fixture: Value = serde_json::from_slice(&std::fs::read(path.join("fixture.json"))?)?;
    let node = fixture["node"].as_str().ok_or_else(|| anyhow::anyhow!("No owned node"))?;
    let guest = VirtualizationGuestIdentity { node: node.into(), vmid: 7101, kind: VirtualizationGuestKind::VirtualMachine };
    let store = Store::open(path.join("power.db"), Config::default().limits)?;
    let start = "00000000-0000-4000-8000-000000000001";
    let shutdown = "00000000-0000-4000-8000-000000000002";
    let stop = "00000000-0000-4000-8000-000000000003";
    let hold = "00000000-0000-4000-8000-000000000004";
    if mode == "seed-hold" {
        let operation = VirtualizationOperation {
            id: hold.parse()?, requested_by: std::fs::read_to_string(path.join("owner-id"))?,
            guest, action: VirtualizationPowerAction::Start, phase: VirtualizationOperationPhase::Submitting,
            task_id: None, error: None, created_at: homecloud_store::now(), updated_at: homecloud_store::now(),
        };
        let (operation, created) = store.reserve_virtualization_operation(operation)?;
        ensure!(created, "Safety-hold fixture already exists");
        return Ok(json!({"seeded_submitting": operation, "synthetic_crash_boundary": true, "backend_not_called": true}));
    }
    let existing = path.join("owner.json").exists();
    let invitation = if existing { None } else { Some(store.bootstrap("Power validation owner")?.1) };
    let config: ProxmoxConnection = serde_json::from_slice(&std::fs::read(filename)?)?;
    let service = Service::new_virtualization(store.clone(), connect_proxmox(config)?)?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .alpns(vec![CONTROL_ALPN.to_vec()]).bind_addr("127.0.0.1:0")?.bind().await?;
    let address = iroh::EndpointAddr::new(endpoint.id()).with_ip_addr(
        endpoint.bound_sockets().into_iter().find(|socket| socket.ip().is_loopback()).unwrap());
    let server = tokio::spawn(homecloud_daemon::serve(service.clone(), endpoint.clone()));
    let outcome = async {
        let owner = if let Some(invitation) = invitation {
            Client::enroll(&path.join("owner.json"), address.clone(), invitation).await?.0
        } else {
            let mut profile = Profile::load(&path.join("owner.json"))?;
            profile.server = address.clone();
            Client::from_profile(profile).await?
        };
        std::fs::write(path.join("owner-id"), owner.device_id())?;
        let value = match mode {
            "start" => submit(&owner, &store, &guest, start, VirtualizationPowerAction::Start).await?,
            "shutdown" => submit(&owner, &store, &guest, shutdown, VirtualizationPowerAction::Shutdown).await?,
            "force-stop" => submit(&owner, &store, &guest, stop, VirtualizationPowerAction::ForceStop).await?,
            "resume" => {
                let before = store.virtualization_operation(shutdown.parse()?)?.unwrap();
                ensure!(before.phase == VirtualizationOperationPhase::Running && before.task_id.is_some(), "Running task lost on restart");
                let replay = owner.virtualization_power(shutdown.parse()?, guest.clone(), VirtualizationPowerAction::Shutdown).await?;
                ensure!(replay.id == before.id && replay.task_id == before.task_id, "Restart replay dispatched a new task");
                let terminal = complete(&owner, shutdown).await?;
                ensure!(terminal.task_id == before.task_id, "UPID changed after restart");
                ensure!(matches!(terminal.phase, VirtualizationOperationPhase::Failed | VirtualizationOperationPhase::Succeeded), "Task did not settle");
                json!({"before": before, "replay": replay, "terminal": terminal, "new_process_service_and_store": true})
            }
            "reader" | "invalid" | "template" => {
                let identifier = "00000000-0000-4000-8000-000000000010";
                let mut target = guest.clone();
                if mode == "template" { target.vmid = 7199; }
                let initial = owner.virtualization_power(identifier.parse()?, target.clone(), VirtualizationPowerAction::Start).await?;
                ensure!(initial.phase == VirtualizationOperationPhase::Submitting, "No durable rejected operation");
                let failed = record(&store, identifier).await?;
                ensure!(failed.phase == VirtualizationOperationPhase::Failed && failed.task_id.is_none(), "Rejected power operation was not Failed");
                let error = failed.error.as_deref().unwrap_or("");
                ensure!(match mode { "reader" => error.contains("403"), "invalid" => error.contains("authentication") || error.contains("401"), _ => error.contains("Templates") }, "Unexpected rejection: {error}");
                let replay = owner.virtualization_power(identifier.parse()?, target, VirtualizationPowerAction::Start).await?;
                ensure!(replay == failed, "Rejected idempotency replay changed record");
                json!({"initial": initial, "failed": failed, "replay": replay})
            }
            "hold" => {
                let interrupted = store.virtualization_operation(hold.parse()?)?.unwrap();
                ensure!(interrupted.phase == VirtualizationOperationPhase::Unknown && interrupted.task_id.is_none(), "Interrupted submission did not become Unknown");
                let blocked = owner.virtualization_power("00000000-0000-4000-8000-000000000005".parse()?, guest.clone(), VirtualizationPowerAction::Start).await.is_err();
                ensure!(blocked, "Unknown safety hold allowed another action");
                let acknowledged = owner.acknowledge_virtualization_operation(interrupted.id).await?;
                ensure!(acknowledged.phase == VirtualizationOperationPhase::Acknowledged, "Hold not acknowledged");
                let replay = owner.virtualization_power(hold.parse()?, guest.clone(), VirtualizationPowerAction::Start).await?;
                ensure!(replay == acknowledged, "Acknowledged key was redispatched");
                json!({"unknown": interrupted, "new_action_blocked": blocked, "acknowledged": acknowledged, "replay": replay})
            }
            "authorization" => {
                let mut container = guest.clone();
                container.kind = VirtualizationGuestKind::Container;
                let container_denied = owner.virtualization_power("00000000-0000-4000-8000-000000000020".parse()?, container, VirtualizationPowerAction::Start).await.is_err();
                let (invitation, _, _) = store.invite(&owner.device_id(), "Power test member", Role::User, Resources::default(), false, 1)?;
                let member = Client::enroll(&path.join("member.json"), address.clone(), invitation).await?.0;
                let unknown = Client::from_profile(Profile { server: address, device_secret: hex::encode(iroh::SecretKey::generate().to_bytes()) }).await?;
                let key = "00000000-0000-4000-8000-000000000021".parse()?;
                let member_denied = member.virtualization_power(key, guest.clone(), VirtualizationPowerAction::Start).await.is_err();
                let unknown_denied = unknown.virtualization_power(key, guest.clone(), VirtualizationPowerAction::Start).await.is_err();
                let member_list_denied = member.virtualization_operations().await.is_err();
                let unknown_list_denied = unknown.virtualization_operations().await.is_err();
                let member_ack_denied = member.acknowledge_virtualization_operation(hold.parse()?).await.is_err();
                store.revoke_device(&owner.device_id(), &owner.device_id())?;
                let revoked_denied = owner.virtualization_power(key, guest, VirtualizationPowerAction::Start).await.is_err();
                let revoked_list_denied = owner.virtualization_operations().await.is_err();
                let revoked_ack_denied = owner.acknowledge_virtualization_operation(hold.parse()?).await.is_err();
                member.close().await;
                unknown.close().await;
                ensure!(container_denied && member_denied && unknown_denied && member_list_denied && unknown_list_denied && member_ack_denied && revoked_denied && revoked_list_denied && revoked_ack_denied, "Authorization failure");
                json!({"container_denied": container_denied, "member_denied": member_denied, "unknown_denied": unknown_denied,
                       "member_list_denied": member_list_denied, "unknown_list_denied": unknown_list_denied, "member_ack_denied": member_ack_denied,
                       "revoked_denied": revoked_denied, "revoked_list_denied": revoked_list_denied, "revoked_ack_denied": revoked_ack_denied})
            }
            _ => anyhow::bail!("Unknown probe mode"),
        };
        owner.close().await;
        Ok(json!({"case": mode, "observations": value, "operations": store.virtualization_operations()?,
                  "legacy_vms_created": store.vms()?.len(), "authenticated_iroh": true}))
    }.await;
    endpoint.close().await;
    server.abort();
    outcome
}

#[tokio::main]
async fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let result = if arguments.len() == 3 { validate(&arguments[1], &arguments[2]).await }
        else { Err(anyhow::anyhow!("Expected mode and private config filename")) };
    match result {
        Ok(value) => println!("{}", json!({"ok": true, "result": value})),
        Err(error) => println!("{}", json!({"ok": false, "error": format!("{error:#}")})),
    }
}
