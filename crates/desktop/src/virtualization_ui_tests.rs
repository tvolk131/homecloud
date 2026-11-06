use super::*;
use crate::{
    virtualization::{Cockpit, Event, GuestIdentity, Page as CockpitPage},
    visual_tests::{lock, simulator},
};

pub(super) fn fixture() -> App {
    let inventory = VirtualizationInventory {
        source: VirtualizationSource::Simulated,
        nodes: vec![VirtualizationNode {
            name: "demo-proxmox".into(),
            status: "online".into(),
            cpu_threads: Some(16),
            cpu_usage_ratio: Some(0.125),
            memory_capacity_bytes: Some(64 * GIB),
            memory_used_bytes: Some(12 * GIB),
        }],
        guests: vec![
            VirtualizationGuest {
                node: "demo-proxmox".into(),
                vmid: 101,
                kind: VirtualizationGuestKind::VirtualMachine,
                name: Some("t3-workstation".into()),
                status: "running".into(),
                template: false,
                cpu_limit: Some(4),
                memory_limit_bytes: Some(8 * GIB),
                memory_used_bytes: Some(3 * GIB),
                reported_disk_capacity_bytes: Some(64 * GIB),
            },
            VirtualizationGuest {
                node: "demo-proxmox".into(),
                vmid: 9000,
                kind: VirtualizationGuestKind::VirtualMachine,
                name: Some("clean-linux-template".into()),
                status: "stopped".into(),
                template: true,
                cpu_limit: Some(2),
                memory_limit_bytes: Some(2 * GIB),
                memory_used_bytes: None,
                reported_disk_capacity_bytes: Some(16 * GIB),
            },
            VirtualizationGuest {
                node: "demo-proxmox".into(),
                vmid: 201,
                kind: VirtualizationGuestKind::Container,
                name: None,
                status: "unknown".into(),
                template: false,
                cpu_limit: None,
                memory_limit_bytes: None,
                memory_used_bytes: None,
                reported_disk_capacity_bytes: None,
            },
        ],
        storage: vec![VirtualizationStorage {
            node: "demo-proxmox".into(),
            name: "local-zfs".into(),
            status: "available".into(),
            capacity_bytes: Some(1024 * GIB),
            used_bytes: Some(80 * GIB),
        }],
    };
    let mut cockpit = Cockpit::default();
    cockpit.receive(Ok(inventory), unix_now());
    let (session, generation, revision) = cockpit.begin_operations_refresh();
    cockpit.update(Event::Operations {
        session,
        generation,
        revision,
        response: Ok(Vec::new()),
    });
    let mut app = App::blank("cockpit-fixture.json".into());
    app.control_plane_mode = Some(ControlPlaneMode::Virtualization);
    app.cockpit = Some(cockpit);
    app.reduced_motion = true;
    app
}

#[test]
fn cockpit_shows_observations_without_legacy_or_unimplemented_actions() {
    let _guard = lock();
    let mut app = fixture();
    let mut ui = simulator(&app, (1180.0, 1000.0));
    for label in [
        "Proxmox cockpit",
        "Simulated · no real guests",
        "Owner power controls",
        "Nodes: 1 · Guests: 3 · Templates: 1",
        "demo-proxmox",
        "CPU: 16 threads · 12.5% usage",
    ] {
        assert!(ui.find(label).is_ok(), "Missing {label}");
    }
    for label in [
        "New VM",
        "People",
        "Activity",
        "Shut down host…",
        "Invite coworker",
    ] {
        assert!(ui.find(label).is_err(), "Unexpected {label}");
    }
    ui.click("VMs").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| matches!(message, Msg::Cockpit(Event::Navigate(CockpitPage::Guests))))
    );
    for message in messages {
        let _ = app.update(message);
    }
    let mut ui = simulator(&app, (1180.0, 1400.0));
    assert!(ui.find("VM template · ID 9000 · demo-proxmox").is_ok());
    assert!(ui.find("Container · ID 201 · demo-proxmox").is_ok());
    assert!(ui.find("Open terminal").is_err());
    assert!(ui.find("Start").is_err());
    assert!(ui.find("Install T3 Code").is_err());
    ui.click("Inspect guest 101").unwrap();
    for message in ui.into_messages() {
        let _ = app.update(message);
    }
    let mut details = simulator(&app, (1180.0, 1000.0));
    assert!(
        details
            .find("Virtual machine · ID 101 · node demo-proxmox")
            .is_ok()
    );
    assert!(details.find("Reported disk capacity: 64.0 GiB").is_ok());
    assert!(details.find("Disk capacity is not guest filesystem usage, a backing-space reservation, or a hard quota.").is_ok());
    assert!(details.find("Shut down").is_ok());
    assert!(details.find("Force stop…").is_ok());
}

#[test]
fn unknown_guest_metrics_are_not_presented_as_zero_usage_or_capacity() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::Cockpit(Event::Inspect(GuestIdentity {
        node: "demo-proxmox".into(),
        vmid: 201,
    })));
    let mut ui = simulator(&app, (800.0, 1100.0));
    for label in [
        "Configured vCPUs: Not reported",
        "Observed memory use: Not reported",
        "Reported disk capacity: Not reported",
        "Configured memory limit: Not reported",
    ] {
        assert!(ui.find(label).is_ok(), "Missing {label}");
    }
    assert!(ui.find("Observed memory use: 0.0 MiB").is_err());
}

#[test]
fn failed_refresh_retains_inventory_with_a_persistent_stale_warning() {
    let _guard = lock();
    let mut app = fixture();
    let original = app.cockpit.as_ref().unwrap().inventory.clone().unwrap();
    app.refresh_generation = 2;
    app.refreshing = true;
    let _ = app.update(Msg::VirtualizationInventory(
        2,
        Err("API unreachable".into()),
    ));
    assert!(!app.refreshing);
    let mut ui = simulator(&app, (1180.0, 1000.0));
    assert!(ui.find("Inventory is stale").is_ok());
    assert!(ui.find("API unreachable").is_ok());
    assert!(ui.find("demo-proxmox").is_ok());
    assert!(ui.find("Dismiss").is_err());
    drop(ui);
    let _ = app.update(Msg::VirtualizationInventory(2, Ok(original.clone())));
    assert_eq!(
        app.cockpit.as_ref().unwrap().inventory.as_ref(),
        Some(&original)
    );
    assert!(!app.cockpit.as_ref().unwrap().stale(unix_now()));
    let mut ui = simulator(&app, (1180.0, 1000.0));
    assert!(ui.find("Inventory is stale").is_err());
}

#[test]
fn stale_or_wrong_mode_responses_cannot_replace_the_current_inventory() {
    let mut app = fixture();
    let original = app.cockpit.as_ref().unwrap().inventory.clone();
    app.refresh_generation = 3;
    app.refreshing = true;
    let _ = app.update(Msg::VirtualizationInventory(2, Err("Old failure".into())));
    let _ = app.update(Msg::ControlPlaneMode(3, Ok(ControlPlaneMode::ManagedHost)));
    let cockpit = app.cockpit.as_ref().unwrap();
    assert_eq!(cockpit.inventory, original);
    assert!(cockpit.error.is_none());
    assert!(app.refreshing);
    assert_eq!(
        app.control_plane_mode,
        Some(ControlPlaneMode::Virtualization)
    );
}

#[test]
fn no_first_sample_is_not_misrepresented_as_an_empty_success() {
    let _guard = lock();
    let mut app = fixture();
    app.cockpit = Some(Cockpit::default());
    let _ = app.update(Msg::VirtualizationInventory(0, Err("Forbidden".into())));
    let mut ui = simulator(&app, (800.0, 1000.0));
    assert!(ui.find("Inventory unavailable").is_ok());
    assert!(ui.find("Forbidden").is_ok());
    assert!(ui.find("No nodes visible to this API token.").is_err());
    assert!(ui.find("Source not yet observed").is_ok());
}

#[test]
fn discovery_is_fail_closed_and_does_not_open_the_create_outbox() {
    let mut app = App::blank("unopened-profile.json".into());
    app.refresh_generation = 7;
    app.refreshing = true;
    let _ = app.update(Msg::ControlPlaneMode(6, Ok(ControlPlaneMode::ManagedHost)));
    assert!(app.control_plane_mode.is_none());
    assert!(app.refreshing);
    let _ = app.update(Msg::ControlPlaneMode(7, Err("Unsupported request".into())));
    assert!(app.control_plane_mode.is_none());
    assert!(app.create_outbox.is_none());
    assert!(app.create_pending.is_none());
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("No saved VM requests were replayed")
    );
    let _ = app.update(Msg::ControlPlaneMode(
        7,
        Ok(ControlPlaneMode::Virtualization),
    ));
    assert!(app.cockpit.is_some());
    assert!(app.create_outbox.is_none());
}

#[test]
fn cockpit_discards_legacy_actions_and_late_legacy_replies() {
    let mut app = fixture();
    for message in [
        Msg::Open(Panel::Create),
        Msg::Create,
        Msg::Invite,
        Msg::Grant,
        Msg::Navigate(Page::People),
        Msg::ShutdownHost,
        Msg::Change(Mutation::Start {
            vm_id: Uuid::new_v4(),
            without_gpu: false,
        }),
        Msg::Reply(Ok(Response::Ok)),
    ] {
        let _ = app.update(message);
    }
    assert!(app.panel.is_none());
    assert!(app.create_outbox.is_none());
    assert!(!app.busy);
    assert!(app.snapshot.is_none());
    assert_eq!(app.cockpit.as_ref().unwrap().page, CockpitPage::Overview);
}

#[test]
fn missing_selected_guest_does_not_imply_it_was_deleted() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::Cockpit(Event::Inspect(GuestIdentity {
        node: "old-node".into(),
        vmid: 101,
    })));
    let mut ui = simulator(&app, (800.0, 1000.0));
    assert!(ui.find("Guest no longer visible here").is_ok());
    assert!(ui.find("It may have moved, been removed, or become hidden by API permissions. No deletion or other action was taken.").is_ok());
    assert!(ui.find("Back to VMs").is_ok());
}

#[test]
fn storage_is_capacity_only_and_an_empty_inventory_remains_permission_scoped() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::Cockpit(Event::Navigate(CockpitPage::Storage)));
    let mut ui = simulator(&app, (800.0, 1000.0));
    assert!(ui.find("local-zfs").is_ok());
    assert!(ui.find("80.0 GiB used / 1024.0 GiB capacity").is_ok());
    for label in ["Replace disk", "Scrub", "Healthy", "Set quota"] {
        assert!(ui.find(label).is_err());
    }
    drop(ui);
    app.cockpit
        .as_mut()
        .unwrap()
        .inventory
        .as_mut()
        .unwrap()
        .storage
        .clear();
    let mut ui = simulator(&app, (800.0, 1000.0));
    assert!(ui.find("No storage visible to this API token.").is_ok());
}

#[test]
fn old_missing_and_future_timestamps_are_stale() {
    let mut cockpit = Cockpit::default();
    assert!(cockpit.stale(100));
    for (received, stale) in [(60, true), (100, false), (101, true)] {
        cockpit.received_at = Some(received);
        assert_eq!(cockpit.stale(100), stale);
    }
}
