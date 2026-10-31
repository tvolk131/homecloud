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
fn partial_smart_errors_distinguish_health_results_without_hiding_failures() {
    let _guard = lock();
    for (passed, expected) in [
        (Some(false), "SMART reports failure"),
        (Some(true), "SMART passed · partial read"),
        (None, "SMART health not reported · partial read"),
    ] {
        let mut app = storage_fixture();
        let inventory = app
            .snapshot
            .as_mut()
            .unwrap()
            .inventory
            .storage
            .as_mut()
            .unwrap();
        inventory.disks.retain(|disk| disk.path == "/dev/nvme0n1");
        inventory.disks[0].smart.passed = passed;
        inventory.disks[0].smart.collection.error = Some("Some SMART commands failed".into());
        let mut ui = simulator(&app, (1180.0, 1900.0));
        assert!(ui.find(expected).is_ok(), "passed={passed:?}");
        assert!(ui.find("SMART passed").is_err());
        ui.click("Inspect /dev/nvme0n1").unwrap();
        let messages = ui.into_messages().collect::<Vec<_>>();
        let _ = app.update(messages[0].clone());
        let mut details = simulator(&app, (1180.0, 1900.0));
        assert!(details.find("Some SMART commands failed").is_ok());
        if passed == Some(false) {
            assert!(details.find("Last device-reported health: FAILED").is_ok());
        }
    }
}

#[test]
fn partial_smart_labels_do_not_hide_stale_or_future_samples() {
    let _guard = lock();
    let now = unix_now();
    for (sample_time, discovery_age, discovery_error) in [
        (Some(now.saturating_sub(1801)), 1, None),
        (Some(now + 60), 1, None),
        (None, 1, None),
        (Some(now.saturating_sub(60)), 181, None),
        (Some(now.saturating_sub(60)), 1, Some("Discovery timed out")),
    ] {
        let mut app = storage_fixture();
        let inventory = app
            .snapshot
            .as_mut()
            .unwrap()
            .inventory
            .storage
            .as_mut()
            .unwrap();
        inventory.disk_collection.observed_at = Some(now.saturating_sub(discovery_age));
        inventory.disk_collection.error = discovery_error.map(str::to_owned);
        inventory.disks.retain(|disk| disk.path == "/dev/nvme0n1");
        let smart = &mut inventory.disks[0].smart;
        smart.passed = Some(true);
        smart.collection.observed_at = sample_time;
        smart.collection.error = Some("Optional SMART log unavailable".into());
        let mut ui = simulator(&app, (1180.0, 1900.0));
        assert!(ui.find("SMART not current").is_ok());
        assert!(ui.find("SMART passed · partial read").is_err());
        assert!(ui.find("SMART passed").is_err());
    }
}

#[test]
fn retained_smart_failures_remain_prominent_without_claiming_a_current_sample() {
    let _guard = lock();
    for (state, sample_age, discovery_age, discovery_error) in [
        (SmartState::Sleeping, Some(1), 1, None),
        (SmartState::Unavailable, Some(1), 1, None),
        (SmartState::Unsupported, Some(1), 1, None),
        (SmartState::Current, Some(1801), 1, None),
        (SmartState::Current, None, 1, None),
        (SmartState::Current, Some(1), 181, None),
        (SmartState::Current, Some(1), 1, Some("Discovery timed out")),
    ] {
        let mut app = storage_fixture();
        let inventory = app
            .snapshot
            .as_mut()
            .unwrap()
            .inventory
            .storage
            .as_mut()
            .unwrap();
        let now = unix_now();
        inventory.disk_collection.observed_at = Some(now.saturating_sub(discovery_age));
        inventory.disk_collection.error = discovery_error.map(str::to_owned);
        inventory.disks.retain(|disk| disk.path == "/dev/nvme0n1");
        let smart = &mut inventory.disks[0].smart;
        smart.state = state;
        smart.collection.observed_at = sample_age.map(|age| now.saturating_sub(age));
        let mut ui = simulator(&app, (1180.0, 1900.0));
        assert!(
            ui.find("SMART reported failure (last sample)").is_ok(),
            "state={state:?}, sample_age={sample_age:?}, discovery_age={discovery_age}, discovery_error={discovery_error:?}"
        );
        assert!(ui.find("SMART reports failure").is_err());
        assert!(ui.find("SMART passed").is_err());
    }
}

#[test]
fn a_new_successful_sample_can_clear_a_retained_failure_warning() {
    let _guard = lock();
    let mut app = storage_fixture();
    {
        let inventory = app
            .snapshot
            .as_mut()
            .unwrap()
            .inventory
            .storage
            .as_mut()
            .unwrap();
        inventory.disks.retain(|disk| disk.path == "/dev/nvme0n1");
        inventory.disks[0].smart.state = SmartState::Sleeping;
        let mut ui = simulator(&app, (1180.0, 1900.0));
        assert!(ui.find("SMART reported failure (last sample)").is_ok());
    }
    let inventory = app
        .snapshot
        .as_mut()
        .unwrap()
        .inventory
        .storage
        .as_mut()
        .unwrap();
    inventory.disks[0].smart.state = SmartState::Current;
    inventory.disks[0].smart.passed = Some(true);
    inventory.disks[0].smart.collection = StorageCollection {
        attempted_at: Some(unix_now()),
        observed_at: Some(unix_now()),
        error: None,
    };
    let mut ui = simulator(&app, (1180.0, 1900.0));
    assert!(ui.find("SMART passed").is_ok());
    assert!(ui.find("SMART reports failure").is_err());
    assert!(ui.find("SMART reported failure (last sample)").is_err());
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
