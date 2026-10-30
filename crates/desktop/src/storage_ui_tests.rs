use super::*;
use crate::visual_tests::{lock, simulator, storage_fixture};

#[test]
fn storage_view_exposes_topology_boot_disks_errors_and_no_destructive_actions() {
    let _guard = lock();
    let app = storage_fixture();
    let mut ui = simulator(&app, (1180.0, 1900.0));
    for label in [
        "Disks & ZFS",
        "Storage observability",
        "Imported ZFS pools",
        "All disks",
        "No redundancy in data vdev /dev/sda1",
        "ZFS errors: read 0 · write 0 · checksum 2",
        "Fixture OS NVMe",
        "OS / boot filesystem reported on this disk.",
        "SMART reports failure",
    ] {
        assert!(ui.find(label).is_ok(), "Missing {label}");
    }
    for label in ["Format", "Replace", "Repair", "Start self-test", "Erase"] {
        assert!(ui.find(label).is_err());
    }
    assert!(ui.find("Dismiss").is_err());
}

#[test]
fn inspecting_a_disk_exposes_identity_metrics_and_pool_membership_and_can_be_closed() {
    let _guard = lock();
    let mut app = storage_fixture();
    app.snapshot
        .as_mut()
        .unwrap()
        .inventory
        .storage
        .as_mut()
        .unwrap()
        .pools
        .clear();
    let mut ui = simulator(&app, (1180.0, 1900.0));
    ui.click("Inspect /dev/nvme0n1").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::StorageDisk(id)] if id == "serial:fixture-boot"));
    let _ = app.update(messages[0].clone());
    let mut ui = simulator(&app, (1180.0, 1900.0));
    for label in [
        "Serial: SYNTHETIC-BOOT",
        "Percentage used: 104",
        "Media / data integrity errors: 2",
        "Temperature: 45 °C",
        "Not matched to an imported pool. This does not mean the disk is unused.",
    ] {
        assert!(ui.find(label).is_ok(), "Missing {label}");
    }
    ui.click("Hide /dev/nvme0n1").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    let _ = app.update(messages[0].clone());
    assert!(app.storage_disk.is_none());
}

#[test]
fn failed_collections_keep_old_values_without_claiming_smart_is_current() {
    let _guard = lock();
    let mut app = storage_fixture();
    let inventory = app
        .snapshot
        .as_mut()
        .unwrap()
        .inventory
        .storage
        .as_mut()
        .unwrap();
    inventory.disk_collection.error = Some("Discovery timed out".into());
    let mut ui = simulator(&app, (1180.0, 1900.0));
    assert!(ui.find("Discovery timed out").is_ok());
    assert!(ui.find("SMART passed").is_err());
    assert!(ui.find("Fixture storage HDD").is_ok());
}

#[test]
fn members_cannot_see_storage_even_if_a_fixture_contains_admin_data() {
    let _guard = lock();
    let mut app = storage_fixture();
    app.snapshot.as_mut().unwrap().me.role = Role::User;
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Disks & ZFS").is_err());
    assert!(
        ui.find("Storage inventory is available to administrators.")
            .is_ok()
    );
    assert!(ui.find("SYNTHETIC-STORAGE").is_err());
    assert!(ui.find("Fixture OS NVMe").is_err());
}

#[test]
fn older_controller_and_pending_inventory_do_not_imply_no_disks_or_pools() {
    let _guard = lock();
    let mut app = storage_fixture();
    app.snapshot.as_mut().unwrap().inventory.storage = None;
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Imported ZFS pools").is_err());
    drop(ui);
    app.snapshot.as_mut().unwrap().inventory.storage = Some(StorageInventory::default());
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("No imported pools reported.").is_err());
    assert!(
        ui.find("No kernel-visible physical disks reported.")
            .is_err()
    );
}
