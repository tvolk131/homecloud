use super::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn disk_payload() -> Value {
    json!({"blockdevices": [
        {"path": "/dev/sda", "type": "disk", "size": 1_000_000_000_000_u64,
         "model": "Fixture HDD", "serial": "SYNTHETIC-A", "wwn": "0x5000a", "tran": "sata", "rota": true,
         "children": [{"path": "/dev/sda1", "type": "part", "fstype": "zfs_member", "mountpoints": [null]}]},
        {"path": "/dev/nvme0n1", "type": "disk", "size": "500000000000",
         "model": "Fixture NVMe", "serial": "SYNTHETIC-BOOT", "tran": "nvme", "rota": false,
         "children": [{"path": "/dev/nvme0n1p1", "type": "part", "fstype": "ext4", "mountpoints": ["/"]}]},
        {"path": "/dev/loop0", "type": "loop", "size": 1000}
    ]})
}

fn mixed_disk_payload(path_only_count: usize) -> Value {
    let mut devices = vec![disk_payload()["blockdevices"][0].clone()];
    devices.extend((0..path_only_count).map(|index| {
        json!({"path": format!("/dev/fixture{index}"), "type": "disk", "model": "QEMU HARDDISK"})
    }));
    json!({"blockdevices": devices})
}

fn duplicate_disk_payload(wwn: bool) -> Value {
    let mut first = disk_payload()["blockdevices"][0].clone();
    first["children"] = json!([]);
    if !wwn {
        first["wwn"] = Value::Null;
    }
    let mut second = first.clone();
    second["path"] = json!("/dev/sdb");
    json!({"blockdevices": [first, second]})
}

fn pool_payload() -> Value {
    json!({"pools": {"tank": {"name": "tank", "pool_guid": 100, "state": "ONLINE",
        "scan_stats": {"function": "scrub", "state": "finished"},
        "vdevs": {"tank": {"name": "tank", "guid": 100, "vdev_type": "root", "state": "ONLINE",
            "vdevs": {"disk": {"name": "/dev/sda1", "guid": 101, "path": "/dev/sda1", "vdev_type": "disk",
                "class": "normal", "state": "ONLINE", "read_errors": 0, "write_errors": 1, "checksum_errors": "2"}}}},
        "special": {"special": {"name": "/dev/sdz1", "guid": 102, "vdev_type": "disk", "state": "UNAVAIL", "not_present": 1}},
        "logs": {"log": {"name": "/dev/sdy1", "vdev_type": "disk", "state": "ONLINE"}},
        "l2cache": {"cache": {"name": "/dev/sdx1", "vdev_type": "disk", "state": "ONLINE"}},
        "spares": {"spare": {"name": "/dev/sdw1", "vdev_type": "disk", "state": "AVAIL"}}
    }}})
}

fn smart_payload() -> Value {
    json!({"smart_status": {"passed": true}, "temperature": {"current": 32}, "power_on_time": {"hours": 100},
        "ata_smart_attributes": {"table": [{"id": 197, "name": "Current_Pending_Sector", "raw": {"value": 2}}]},
        "ata_smart_self_test_log": {"standard": {"table": [{"type": {"string": "Short offline"}, "status": {"string": "Completed without error"}}]}}})
}

struct Fixture {
    disks: Value,
    pools: Value,
    smart: Value,
    smart_code: i32,
    failures: Vec<&'static str>,
    calls: Vec<Probe>,
    active: usize,
    maximum_active: usize,
}

#[derive(Clone)]
struct FixtureRunner(Arc<Mutex<Fixture>>);

impl FixtureRunner {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Fixture {
            disks: disk_payload(),
            pools: pool_payload(),
            smart: smart_payload(),
            smart_code: 0,
            failures: Vec::new(),
            calls: Vec::new(),
            active: 0,
            maximum_active: 0,
        })))
    }

    fn smart_calls(&self) -> usize {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|probe| matches!(probe, Probe::Smart { .. }))
            .count()
    }
}

#[async_trait]
impl Runner for FixtureRunner {
    async fn run(&self, probe: Probe) -> Result<ProbeOutput> {
        let smart = matches!(probe, Probe::Smart { .. });
        let (payload, code) = {
            let mut fixture = self.0.lock().unwrap();
            let program = probe.command().0;
            fixture.calls.push(probe.clone());
            ensure!(
                !fixture.failures.contains(&program),
                "Fixture {program} failure"
            );
            if smart {
                fixture.active += 1;
                fixture.maximum_active = fixture.maximum_active.max(fixture.active);
                (fixture.smart.clone(), fixture.smart_code)
            } else if matches!(probe, Probe::Disks) {
                (fixture.disks.clone(), 0)
            } else {
                (fixture.pools.clone(), 0)
            }
        };
        if smart {
            tokio::time::sleep(Duration::from_millis(1)).await;
            self.0.lock().unwrap().active -= 1;
        }
        Ok(ProbeOutput {
            code,
            stdout: serde_json::to_vec(&payload)?,
            stderr: Vec::new(),
        })
    }
}

#[test]
fn physical_inventory_includes_boot_mounts_partitions_and_stable_ids() {
    let disks = parse::disks(&disk_payload()).unwrap();
    assert_eq!(disks.len(), 2);
    assert_eq!(disks[0].id, "wwn:0x5000a");
    assert_eq!(disks[0].devices[1].path, "/dev/sda1");
    assert_eq!(disks[1].devices[1].mountpoints, ["/"]);
    assert_eq!(disks[1].rotational, Some(false));
    assert_eq!(disks[1].size_bytes, Some(500_000_000_000));
    assert_eq!(disks[0].smart.state, SmartState::Pending);
}

#[test]
fn disk_discovery_requests_kernel_device_names() {
    let (program, args, _) = Probe::Disks.command();
    assert_eq!(program, "lsblk");
    let columns = args
        .windows(2)
        .find(|arguments| arguments[0] == "--output")
        .unwrap();
    assert!(columns[1].split(',').any(|column| column == "KNAME"));
}

#[test]
fn zvols_are_excluded_by_kernel_name_with_a_device_path_fallback() {
    for (path, kernel_name) in [
        ("/dev/zd0", Value::Null),
        ("/dev/zd16", json!("zd16")),
        ("/dev/zd16384", json!("/dev/zd16384")),
        ("/dev/zvol/tank/guest", json!("/dev/zd32")),
    ] {
        let mut payload = disk_payload();
        payload["blockdevices"].as_array_mut().unwrap().push(json!({
            "path": path, "kname": kernel_name, "type": "disk", "size": 32_000_000_000_u64,
            "model": null, "serial": null, "wwn": null,
            "children": [{"path": format!("{path}p1"), "type": "part", "fstype": "ext4"}]
        }));
        let disks = parse::disks(&payload).unwrap();
        assert_eq!(disks.len(), 2, "zvol {path} was counted as a physical disk");
        assert_eq!(disks[0].devices[1].path, "/dev/sda1");
        assert_eq!(disks[1].devices[1].mountpoints, ["/"]);
    }
}

#[test]
fn guest_disks_and_disks_without_hardware_metadata_remain_discoverable() {
    let payload = json!({"blockdevices": [
        {"path": "/dev/sda", "kname": "/dev/sda", "type": "disk", "model": "QEMU HARDDISK"},
        {"path": "/dev/vda", "type": "disk", "model": null, "serial": null, "wwn": null},
        {"path": "/dev/xvda", "type": "disk", "fstype": "zfs_member"},
        {"path": "/dev/disk/by-id/zd0", "kname": "/dev/nvme0n1", "type": "disk"},
        {"path": "/dev/zda", "type": "disk"},
        {"path": "/dev/zd", "type": "disk"},
        {"path": "/dev/zd0-other", "type": "disk"}
    ]});
    let disks = parse::disks(&payload).unwrap();
    assert_eq!(disks.len(), 7);
    assert_eq!(disks[0].model.as_deref(), Some("QEMU HARDDISK"));
    assert!(disks[1].model.is_none());
    assert!(disks[1].serial.is_none());
    assert!(disks[1].wwn.is_none());
    assert_eq!(
        disks[2].devices[0].filesystem.as_deref(),
        Some("zfs_member")
    );
}

#[test]
fn logical_vdev_leaves_remain_visible_without_a_physical_disk_match() {
    let payload = json!({"blockdevices": [{"path": "/dev/zd0", "type": "disk"}]});
    let disks = parse::disks(&payload).unwrap();
    let mut pools = pool_payload();
    pools["pools"]["tank"]["vdevs"]["tank"]["vdevs"]["disk"]["path"] = json!("/dev/zd0");
    let pools = parse::pools(&pools, &disks, "tank").unwrap();
    let leaf = &pools[0].vdevs[0].children[0];
    assert_eq!(leaf.path.as_deref(), Some("/dev/zd0"));
    assert_eq!(leaf.state, "ONLINE");
    assert_eq!(leaf.guid, "101");
    assert!(leaf.disk_id.is_none());
}

#[test]
fn collection_errors_preserve_actionable_causes_and_remain_bounded() {
    let error = anyhow::anyhow!("Permission denied").context("Starting smartctl");
    assert_eq!(
        collection_error(&error),
        "Starting smartctl: Permission denied"
    );
    let error = anyhow::anyhow!("é".repeat(1024)).context("Invalid command output");
    let message = collection_error(&error);
    assert!(message.starts_with("Invalid command output: "));
    assert_eq!(message.chars().count(), 512);
}

#[test]
fn identities_are_unique_and_invalid_paths_are_rejected() {
    let mut payload = disk_payload();
    let mut duplicate = payload["blockdevices"][0].clone();
    duplicate["path"] = json!("/dev/sdb");
    payload["blockdevices"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let disks = parse::disks(&payload).unwrap();
    assert_eq!(disks[0].id, "path:/dev/sda");
    assert_eq!(disks[2].id, "path:/dev/sdb");
    payload["blockdevices"][0]["path"] = json!("/dev/../tmp/invalid");
    assert!(parse::disks(&payload).is_err());
}

#[test]
fn all_colliding_disk_identities_use_paths_independently_of_enumeration_order() {
    for wwn in [true, false] {
        let mut payload = duplicate_disk_payload(wwn);
        for _ in 0..2 {
            let disks = parse::disks(&payload).unwrap();
            for disk in disks {
                assert_eq!(disk.id, format!("path:{}", disk.path));
                assert_eq!(disk.serial.as_deref(), Some("SYNTHETIC-A"));
            }
            payload["blockdevices"].as_array_mut().unwrap().reverse();
        }
    }
    let mut payload = duplicate_disk_payload(true);
    payload["blockdevices"][1]["wwn"] = json!("0x5000b");
    let disks = parse::disks(&payload).unwrap();
    assert_eq!(disks[0].id, "wwn:0x5000a");
    assert_eq!(disks[1].id, "wwn:0x5000b");
}

#[test]
fn partition_membership_roles_counters_and_missing_guids_are_preserved() {
    let disks = parse::disks(&disk_payload()).unwrap();
    let pools = parse::pools(&pool_payload(), &disks, "tank").unwrap();
    let pool = &pools[0];
    let leaf = &pool.vdevs[0].children[0];
    assert_eq!(pool.guid, "100");
    assert_eq!(leaf.disk_id.as_deref(), Some(disks[0].id.as_str()));
    assert_eq!(leaf.role, "data");
    assert_eq!(leaf.checksum_errors, Some(2));
    for role in ["special", "log", "cache", "spare"] {
        assert!(pool.vdevs.iter().any(|vdev| vdev.role == role));
    }
    let missing = pool
        .vdevs
        .iter()
        .find(|vdev| vdev.role == "special")
        .unwrap();
    assert_eq!(missing.guid, "102");
    assert!(missing.disk_id.is_none());
    assert_eq!(pool.warnings.len(), 2);
    assert!(pool.managed);
    assert_eq!(pool.scan.as_deref(), Some("scrub · finished"));
}

#[test]
fn redundant_vdevs_are_not_flattened_into_single_disk_warnings() {
    for kind in ["mirror", "raidz"] {
        let mut payload = pool_payload();
        let root = &mut payload["pools"]["tank"]["vdevs"]["tank"];
        let leaf = root["vdevs"]["disk"].clone();
        root["vdevs"] =
            json!({"group": {"name": "group-0", "vdev_type": kind, "vdevs": [leaf.clone(), leaf]}});
        let pool = parse::pools(&payload, &[], "other").unwrap().remove(0);
        assert_eq!(pool.vdevs[0].children[0].children.len(), 2);
        assert_eq!(pool.warnings.len(), 1);
        assert!(!pool.managed);
    }
}

#[test]
fn ambiguous_and_missing_membership_is_not_guessed() {
    let mut disks = parse::disks(&disk_payload()).unwrap();
    let mut duplicate = disks[0].clone();
    duplicate.id = "another-disk".into();
    disks.push(duplicate);
    let pool = parse::pools(&pool_payload(), &disks, "tank")
        .unwrap()
        .remove(0);
    assert!(pool.vdevs[0].children[0].disk_id.is_none());
    for marker in ["not_present", "was"] {
        let mut payload = pool_payload();
        let leaf = &mut payload["pools"]["tank"]["vdevs"]["tank"]["vdevs"]["disk"];
        leaf[marker] = if marker == "was" {
            json!("/dev/sda1")
        } else {
            json!(1)
        };
        let pool = parse::pools(&payload, &disks[..2], "tank")
            .unwrap()
            .remove(0);
        let leaf = &pool.vdevs[0].children[0];
        assert!(leaf.disk_id.is_none());
        assert_eq!(leaf.path.as_deref(), Some("/dev/sda1"));
        assert_eq!(leaf.guid, "101");
    }
}

#[test]
fn by_id_symlinks_resolve_to_the_containing_disk() {
    let directory = tempfile::tempdir().unwrap();
    let link = directory.path().join("fixture-part1");
    std::os::unix::fs::symlink("/dev/null", &link).unwrap();
    let mut disks = parse::disks(&disk_payload()).unwrap();
    disks[0].devices[1].path = "/dev/null".into();
    let mut payload = pool_payload();
    payload["pools"]["tank"]["vdevs"]["tank"]["vdevs"]["disk"]["path"] = json!(link);
    let pool = parse::pools(&payload, &disks, "tank").unwrap().remove(0);
    assert_eq!(
        pool.vdevs[0].children[0].disk_id.as_deref(),
        Some(disks[0].id.as_str())
    );
}

#[test]
fn file_vdevs_and_unreported_counters_remain_explicit() {
    let payload = json!({"pools": [{"name": "scratch", "vdevs": [{"name": "/tmp/vdev", "vdev_type": "file"}]}]});
    let pool = parse::pools(&payload, &[], "tank").unwrap().remove(0);
    assert_eq!(pool.vdevs[0].kind, "file");
    assert!(pool.vdevs[0].disk_id.is_none());
    assert!(pool.vdevs[0].read_errors.is_none());
    assert_eq!(pool.warnings.len(), 1);
}

#[test]
fn smart_health_and_log_exit_bits_are_not_probe_failures() {
    let mut payload = smart_payload();
    payload["smart_status"]["passed"] = json!(false);
    let result = parse::smart(
        &payload,
        8 | 16 | 64 | 128,
        100,
        SmartObservation::default(),
    );
    assert_eq!(result.state, SmartState::Current);
    assert_eq!(result.passed, Some(false));
    assert_eq!(result.metrics[0].value, "2");
    assert!(result.self_tests[0].contains("Completed without error"));
    assert!(result.collection.error.is_none());
}

#[test]
fn partial_smart_failure_is_retained_when_later_probes_fail_or_skip() {
    let mut payload = smart_payload();
    payload["smart_status"]["passed"] = json!(false);
    let failed = parse::smart(&payload, 4 | 8, 100, SmartObservation::default());
    assert_eq!(failed.state, SmartState::Current);
    assert_eq!(failed.passed, Some(false));
    assert_eq!(failed.collection.observed_at, Some(100));
    assert!(failed.collection.error.is_some());
    for (code, state) in [
        (99, SmartState::Sleeping),
        (98, SmartState::Unsupported),
        (1, SmartState::Unavailable),
    ] {
        let later = parse::skipped_smart(code, failed.clone(), 200)
            .unwrap_or_else(|| parse::smart(&json!({}), code, 200, failed.clone()));
        assert_eq!(later.state, state);
        assert_eq!(later.passed, Some(false));
        assert_eq!(later.collection.observed_at, Some(100));
        assert_eq!(later.collection.attempted_at, Some(200));
    }
}

#[test]
fn missing_health_in_a_partial_read_cannot_clear_a_previous_smart_failure() {
    let mut payload = smart_payload();
    payload["smart_status"]["passed"] = json!(false);
    let failed = parse::smart(&payload, 8, 100, SmartObservation::default());
    for code in [0, 4] {
        for message in [None, Some("SMART Status command failed")] {
            let mut partial = json!({"temperature": {"current": 60}});
            if let Some(message) = message {
                partial["smartctl"] = json!({"messages": [{"string": message}]});
            }
            let mut previous = failed.clone();
            for now in [200, 300] {
                let retained = parse::smart(&partial, code, now, previous);
                assert_eq!(retained.state, SmartState::Unavailable);
                assert_eq!(retained.passed, Some(false));
                assert_eq!(retained.temperature_celsius, Some(32));
                assert_eq!(retained.collection.observed_at, Some(100));
                assert_eq!(retained.collection.attempted_at, Some(now));
                assert_eq!(
                    retained.collection.error.as_deref(),
                    Some(message.unwrap_or(
                        "SMART health status not returned; last reported failure retained"
                    ))
                );
                previous = retained;
            }
            let recovered = parse::smart(&smart_payload(), 4, 400, previous);
            assert_eq!(recovered.state, SmartState::Current);
            assert_eq!(recovered.passed, Some(true));
            assert_eq!(recovered.collection.observed_at, Some(400));
            assert!(recovered.collection.error.is_some());
        }
    }
}

#[test]
fn nvme_large_counters_and_unknown_health_are_preserved_exactly() {
    let payload: Value = serde_json::from_str(r#"{"nvme_smart_health_information_log":{"percentage_used":104,"critical_warning":4,"media_errors":2,"data_units_written":18446744073709551616},"nvme_self_test_log":{"table":[{"self_test_result":{"string":"Completed without error"}}]}}"#).unwrap();
    let result = parse::smart(&payload, 0, 100, SmartObservation::default());
    assert!(result.passed.is_none());
    assert!(result.temperature_celsius.is_none());
    assert!(
        result
            .metrics
            .iter()
            .any(|metric| metric.name == "Percentage used" && metric.value == "104")
    );
    assert!(result.metrics.iter().any(
        |metric| metric.name == "Data units written" && metric.value == "18446744073709551616"
    ));
    assert_eq!(
        result.self_tests,
        ["NVMe self-test: Completed without error"]
    );
}

#[test]
fn failures_and_skips_keep_previous_readings_without_refreshing_success() {
    let previous = parse::smart(&smart_payload(), 0, 100, SmartObservation::default());
    for (code, state) in [
        (2, SmartState::Unavailable),
        (98, SmartState::Unsupported),
        (99, SmartState::Sleeping),
    ] {
        let result = parse::skipped_smart(code, previous.clone(), 200)
            .unwrap_or_else(|| parse::smart(&json!({}), code, 200, previous.clone()));
        assert_eq!(result.state, state);
        assert_eq!(result.collection.observed_at, Some(100));
        assert_eq!(result.collection.attempted_at, Some(200));
        assert!(result.collection.error.is_some());
        assert_eq!(result.temperature_celsius, Some(32));
    }
}

#[test]
fn unsupported_empty_and_partial_smart_do_not_invent_healthy_readings() {
    let unsupported = parse::smart(
        &json!({"smart_support": {"available": false}}),
        0,
        100,
        SmartObservation::default(),
    );
    assert_eq!(unsupported.state, SmartState::Unsupported);
    let empty = parse::smart(&json!({}), 0, 100, SmartObservation::default());
    assert_eq!(empty.state, SmartState::Unavailable);
    let partial = parse::smart(&smart_payload(), 4, 100, SmartObservation::default());
    assert_eq!(partial.state, SmartState::Current);
    assert!(partial.collection.error.is_some());
}

#[tokio::test]
async fn serial_mismatch_discards_previous_sample() {
    let runner = FixtureRunner::new();
    runner.0.lock().unwrap().smart["serial_number"] = json!("OTHER");
    let disk = parse::disks(&disk_payload()).unwrap().remove(0);
    let previous = parse::smart(&smart_payload(), 0, 100, SmartObservation::default());
    let result = probe_smart(runner, &disk, previous, 200).await;
    assert_eq!(result.state, SmartState::Unavailable);
    assert!(result.passed.is_none());
    assert!(result.collection.observed_at.is_none());
    assert!(result.temperature_celsius.is_none());
}

#[tokio::test]
async fn low_power_probe_has_no_active_fallback_or_self_test() {
    let runner = FixtureRunner::new();
    runner.0.lock().unwrap().smart_code = 99;
    let disk = parse::disks(&disk_payload()).unwrap().remove(0);
    let result = probe_smart(runner.clone(), &disk, SmartObservation::default(), 100).await;
    assert_eq!(result.state, SmartState::Sleeping);
    let fixture = runner.0.lock().unwrap();
    assert_eq!(fixture.calls.len(), 1);
    let (program, args, timeout) = fixture.calls[0].command();
    assert_eq!(program, "smartctl");
    assert_eq!(args, ["-j", "-x", "-n", "standby,99,98", "/dev/sda"]);
    assert_eq!(timeout, Duration::from_secs(8));
    let (_, args, _) = Probe::Smart {
        path: "/dev/nvme0n1".into(),
        nvme: true,
    }
    .command();
    assert_eq!(args, ["-j", "-x", "/dev/nvme0n1"]);
}

#[tokio::test]
async fn smart_cache_survives_restarts_and_refreshes_on_expiry_and_clock_rollback() {
    let runner = FixtureRunner::new();
    let state = tempfile::tempdir().unwrap();
    collect_cached(runner.clone(), state.path(), "tank", 1000)
        .await
        .unwrap();
    assert_eq!(runner.smart_calls(), 2);
    collect_cached(runner.clone(), state.path(), "tank", 1060)
        .await
        .unwrap();
    assert_eq!(runner.smart_calls(), 2);
    collect_cached(runner.clone(), state.path(), "tank", 2000)
        .await
        .unwrap();
    assert_eq!(runner.smart_calls(), 4);
    collect_cached(runner.clone(), state.path(), "tank", 900)
        .await
        .unwrap();
    assert_eq!(runner.smart_calls(), 6);
    let record: Cache =
        serde_json::from_slice(&std::fs::read(state.path().join("storage.json")).unwrap()).unwrap();
    assert_eq!(record.version, 1);
    assert_eq!(record.inventory.disk_collection.observed_at, Some(900));
    assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn discovery_and_topology_fail_independently_and_retain_last_success() {
    let runner = FixtureRunner::new();
    let first = observe(
        runner.clone(),
        "tank",
        StorageInventory::default(),
        100,
        SMART_BATCH,
    )
    .await;
    runner.0.lock().unwrap().failures = vec!["lsblk"];
    let disk_failure = observe(runner.clone(), "tank", first.clone(), 200, SMART_BATCH).await;
    assert_eq!(disk_failure.disk_collection.observed_at, Some(100));
    assert_eq!(disk_failure.pool_collection.observed_at, Some(200));
    assert_eq!(disk_failure.disks.len(), 2);
    assert!(disk_failure.pools[0].vdevs[0].children[0].disk_id.is_none());
    runner.0.lock().unwrap().failures = vec!["zpool"];
    let pool_failure = observe(runner, "tank", first.clone(), 200, SMART_BATCH).await;
    assert_eq!(pool_failure.pool_collection.observed_at, Some(100));
    assert_eq!(pool_failure.disk_collection.observed_at, Some(200));
    assert_eq!(
        serde_json::to_value(pool_failure.pools).unwrap(),
        serde_json::to_value(first.pools).unwrap()
    );
}

#[tokio::test]
async fn bounded_batches_prioritize_unsampled_disks() {
    let runner = FixtureRunner::new();
    let first = observe(runner.clone(), "tank", StorageInventory::default(), 100, 1).await;
    assert_eq!(runner.smart_calls(), 1);
    assert_eq!(first.disks[1].smart.state, SmartState::Pending);
    assert!(!first.warnings.is_empty());
    let second = observe(runner.clone(), "tank", first, 160, 1).await;
    assert_eq!(runner.smart_calls(), 2);
    assert_eq!(second.disks[0].smart.collection.observed_at, Some(100));
    assert_eq!(second.disks[1].smart.collection.observed_at, Some(160));
}

#[tokio::test]
async fn path_only_disks_do_not_starve_due_identified_disks() {
    for path_only_count in [31, 32, 40, 64, 96] {
        let runner = FixtureRunner::new();
        runner.0.lock().unwrap().disks = mixed_disk_payload(path_only_count);
        let mut stable = parse::disks(&disk_payload()).unwrap().remove(0);
        stable.smart = parse::smart(&smart_payload(), 0, 100, SmartObservation::default());
        let mut inventory = StorageInventory {
            disks: vec![stable],
            ..Default::default()
        };
        for cycle in 0..=path_only_count / SMART_BATCH {
            let calls_before = runner.smart_calls();
            inventory = observe(
                runner.clone(),
                "tank",
                inventory,
                1000 + cycle as u64 * 60,
                SMART_BATCH,
            )
            .await;
            assert!(runner.smart_calls() - calls_before <= SMART_BATCH);
        }
        let fixture = runner.0.lock().unwrap();
        let mut attempts = BTreeMap::<&str, usize>::new();
        for probe in &fixture.calls {
            if let Probe::Smart { path, .. } = probe {
                *attempts.entry(path).or_default() += 1;
            }
        }
        assert_eq!(
            attempts.get("/dev/sda"),
            Some(&1),
            "{path_only_count} path-only disks"
        );
        assert_eq!(attempts.len(), path_only_count + 1);
        assert!(inventory.disks[0].smart.collection.observed_at.unwrap() >= 1000);
        assert!(fixture.maximum_active <= SMART_WORKERS);
    }
}

#[tokio::test]
async fn path_only_scheduling_survives_cache_reload_without_reusing_readings() {
    let runner = FixtureRunner::new();
    runner.0.lock().unwrap().disks = mixed_disk_payload(40);
    let state = tempfile::tempdir().unwrap();
    collect_cached(runner.clone(), state.path(), "tank", 100)
        .await
        .unwrap();
    let second = collect_cached(runner.clone(), state.path(), "tank", 160)
        .await
        .unwrap();
    let pending: Vec<_> = second
        .disks
        .iter()
        .filter(|disk| disk.smart.state == SmartState::Pending)
        .collect();
    assert_eq!(pending.len(), 8);
    for disk in pending {
        assert_eq!(disk.smart.collection.attempted_at, Some(100));
        assert!(disk.smart.collection.observed_at.is_none());
        assert!(disk.smart.collection.error.is_none());
        assert!(disk.smart.passed.is_none());
        assert!(disk.smart.temperature_celsius.is_none());
        assert!(disk.smart.power_on_hours.is_none());
        assert!(disk.smart.metrics.is_empty());
        assert!(disk.smart.self_tests.is_empty());
    }
    let fixture = runner.0.lock().unwrap();
    let attempted: std::collections::BTreeSet<_> = fixture
        .calls
        .iter()
        .filter_map(|probe| match probe {
            Probe::Smart { path, .. } => Some(path),
            _ => None,
        })
        .collect();
    assert_eq!(attempted.len(), 41);
}

#[tokio::test]
async fn path_only_metadata_changes_discard_scheduling_hints() {
    for changed_field in [None, Some("model"), Some("size")] {
        let runner = FixtureRunner::new();
        runner.0.lock().unwrap().disks = mixed_disk_payload(1);
        let first = observe(
            runner.clone(),
            "tank",
            StorageInventory::default(),
            100,
            SMART_BATCH,
        )
        .await;
        if let Some(field) = changed_field {
            runner.0.lock().unwrap().disks["blockdevices"][1][field] = if field == "model" {
                json!("Replacement disk")
            } else {
                json!(1234)
            };
        }
        let second = observe(runner, "tank", first, 160, 0).await;
        let disk = &second.disks[1];
        assert_eq!(
            disk.smart.collection.attempted_at,
            changed_field.is_none().then_some(100)
        );
        assert!(disk.smart.collection.observed_at.is_none());
        assert!(disk.smart.passed.is_none());
        assert_eq!(disk.smart.state, SmartState::Pending);
    }
}

#[tokio::test]
async fn future_scheduling_hints_cannot_starve_disks_after_clock_rollback() {
    let runner = FixtureRunner::new();
    let payload = mixed_disk_payload(32);
    let mut disks = parse::disks(&payload).unwrap();
    for disk in &mut disks {
        let attempted_at = if disk.serial.is_some() { 1000 } else { 900 };
        disk.smart = parse::smart(
            &smart_payload(),
            0,
            attempted_at,
            SmartObservation::default(),
        );
    }
    runner.0.lock().unwrap().disks = payload;
    let first = observe(
        runner.clone(),
        "tank",
        StorageInventory {
            disks,
            ..Default::default()
        },
        100,
        SMART_BATCH,
    )
    .await;
    assert_eq!(first.disks[0].smart.collection.observed_at, Some(100));
    let second = observe(runner.clone(), "tank", first, 160, SMART_BATCH).await;
    assert!(second.disks.iter().all(|disk| {
        disk.smart
            .collection
            .attempted_at
            .is_some_and(|attempted| attempted <= 160)
    }));
}

#[tokio::test]
async fn zvols_leave_the_inventory_without_consuming_the_smart_budget() {
    let runner = FixtureRunner::new();
    {
        let mut fixture = runner.0.lock().unwrap();
        let mut devices: Vec<_> = (0..40)
            .map(|index| json!({"path": format!("/dev/zd{index}"), "type": "disk"}))
            .collect();
        devices.extend(disk_payload()["blockdevices"].as_array().unwrap().clone());
        fixture.disks = json!({"blockdevices": devices});
    }
    let previous = StorageInventory {
        disks: vec![PhysicalDisk {
            id: "path:/dev/zd0".into(),
            path: "/dev/zd0".into(),
            smart: parse::smart(&smart_payload(), 0, 50, SmartObservation::default()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let result = observe(runner.clone(), "tank", previous, 100, 2).await;
    assert_eq!(result.disks.len(), 2);
    assert_eq!(result.disk_collection.observed_at, Some(100));
    assert!(result.disk_collection.error.is_none());
    assert!(result.warnings.is_empty());
    assert_eq!(runner.smart_calls(), 2);
    assert!(
        result
            .disks
            .iter()
            .all(|disk| disk.smart.state == SmartState::Current)
    );
    let fixture = runner.0.lock().unwrap();
    let mut paths: Vec<_> = fixture
        .calls
        .iter()
        .filter_map(|probe| match probe {
            Probe::Smart { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    paths.sort_unstable();
    assert_eq!(paths, ["/dev/nvme0n1", "/dev/sda"]);
    assert_eq!(
        result.pools[0].vdevs[0].children[0].disk_id.as_deref(),
        Some(result.disks[0].id.as_str())
    );
}

#[tokio::test]
async fn smart_probe_concurrency_and_batch_size_are_bounded() {
    let runner = FixtureRunner::new();
    let disks: Vec<_> = (0..40).map(|index| json!({"path": format!("/dev/fixture{index}"), "type": "disk", "serial": format!("FIXTURE-{index}")})).collect();
    runner.0.lock().unwrap().disks = json!({"blockdevices": disks});
    let result = observe(
        runner.clone(),
        "tank",
        StorageInventory::default(),
        100,
        SMART_BATCH,
    )
    .await;
    assert_eq!(runner.smart_calls(), 32);
    assert!(runner.0.lock().unwrap().maximum_active <= 4);
    assert_eq!(
        result
            .disks
            .iter()
            .filter(|disk| disk.smart.state == SmartState::Pending)
            .count(),
        8
    );
    let result = observe(runner.clone(), "tank", result, 160, SMART_BATCH).await;
    assert_eq!(runner.smart_calls(), 40);
    assert!(
        result
            .disks
            .iter()
            .all(|disk| disk.smart.state == SmartState::Current)
    );
}

#[tokio::test]
async fn replacement_disk_and_path_only_identity_do_not_inherit_cached_smart() {
    for stable in [true, false] {
        let runner = FixtureRunner::new();
        if !stable {
            let mut fixture = runner.0.lock().unwrap();
            fixture.disks["blockdevices"][0]["serial"] = Value::Null;
            fixture.disks["blockdevices"][0]["wwn"] = Value::Null;
        }
        let first = observe(
            runner.clone(),
            "tank",
            StorageInventory::default(),
            100,
            SMART_BATCH,
        )
        .await;
        if stable {
            let mut fixture = runner.0.lock().unwrap();
            fixture.disks["blockdevices"][0]["serial"] = json!("REPLACEMENT");
            fixture.disks["blockdevices"][0]["wwn"] = json!("0x5000b");
        }
        let second = observe(runner.clone(), "tank", first, 160, SMART_BATCH).await;
        assert_eq!(runner.smart_calls(), 3);
        assert_eq!(second.disks[0].smart.collection.observed_at, Some(160));
    }
}

#[tokio::test]
async fn colliding_identities_do_not_reuse_health_after_reordering_or_removal() {
    for wwn in [true, false] {
        for remove_first in [true, false] {
            let runner = FixtureRunner::new();
            runner.0.lock().unwrap().disks = duplicate_disk_payload(wwn);
            let mut first = observe(
                runner.clone(),
                "tank",
                StorageInventory::default(),
                100,
                SMART_BATCH,
            )
            .await;
            first.disks[0].smart.passed = Some(false);
            {
                let mut fixture = runner.0.lock().unwrap();
                let devices = fixture.disks["blockdevices"].as_array_mut().unwrap();
                if remove_first {
                    devices.remove(0);
                } else {
                    devices.reverse();
                }
                fixture.smart_code = 99;
            }
            let second = observe(runner.clone(), "tank", first, 160, SMART_BATCH).await;
            assert_eq!(runner.smart_calls(), 2 + second.disks.len());
            for disk in second.disks {
                assert_eq!(disk.smart.state, SmartState::Sleeping);
                assert!(disk.smart.passed.is_none());
                assert!(disk.smart.collection.observed_at.is_none());
                assert_eq!(disk.smart.collection.attempted_at, Some(160));
            }
        }
    }
}

#[tokio::test]
async fn legacy_ambiguous_cache_cannot_assign_a_removed_disks_health_to_its_survivor() {
    for wwn in [true, false] {
        let runner = FixtureRunner::new();
        let mut payload = duplicate_disk_payload(wwn);
        let mut previous = StorageInventory {
            disks: parse::disks(&payload).unwrap(),
            ..Default::default()
        };
        let identity = if wwn {
            "wwn:0x5000a"
        } else {
            "serial:Fixture HDD:SYNTHETIC-A"
        };
        previous.disks[0].id = identity.into();
        previous.disks[1].id = format!("{identity}:/dev/sdb");
        for disk in &mut previous.disks {
            disk.smart = parse::smart(&smart_payload(), 0, 100, SmartObservation::default());
        }
        previous.disks[0].smart.passed = Some(false);
        let state = tempfile::tempdir().unwrap();
        save_cache(state.path(), &previous).unwrap();
        payload["blockdevices"].as_array_mut().unwrap().remove(0);
        {
            let mut fixture = runner.0.lock().unwrap();
            fixture.disks = payload;
            fixture.smart_code = 99;
        }
        let current = collect_cached(runner.clone(), state.path(), "tank", 160)
            .await
            .unwrap();
        assert_eq!(runner.smart_calls(), 1);
        assert_eq!(current.disks[0].smart.state, SmartState::Sleeping);
        assert!(current.disks[0].smart.passed.is_none());
        assert!(current.disks[0].smart.collection.observed_at.is_none());
    }
}

#[tokio::test]
async fn missing_tools_and_malformed_sources_are_not_successful_empty_inventory() {
    for malformed in [true, false] {
        let runner = FixtureRunner::new();
        {
            let mut fixture = runner.0.lock().unwrap();
            if malformed {
                fixture.disks = json!({});
                fixture.pools = json!({});
            } else {
                fixture.failures = vec!["lsblk", "zpool", "smartctl"];
            }
        }
        let result = observe(
            runner,
            "tank",
            StorageInventory::default(),
            100,
            SMART_BATCH,
        )
        .await;
        assert!(result.disk_collection.observed_at.is_none());
        assert!(result.pool_collection.observed_at.is_none());
        assert!(result.disk_collection.error.is_some());
        assert!(result.pool_collection.error.is_some());
    }
}

#[tokio::test]
async fn cache_locks_serialize_collectors_and_preserve_probe_ttl() {
    let runner = FixtureRunner::new();
    let state = tempfile::tempdir().unwrap();
    let (first, second) = tokio::join!(
        collect_cached(runner.clone(), state.path(), "tank", 100),
        collect_cached(runner.clone(), state.path(), "tank", 100)
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(runner.smart_calls(), 2);
}

#[test]
fn corrupt_or_unknown_cache_versions_do_not_fabricate_observations() {
    let state = tempfile::tempdir().unwrap();
    for bytes in [
        b"not json".as_slice(),
        br#"{"version":2,"inventory":{"disks":[{}]}}"#,
    ] {
        std::fs::write(state.path().join("storage.json"), bytes).unwrap();
        let inventory = load_cache(state.path());
        assert!(inventory.disks.is_empty());
        assert!(inventory.disk_collection.observed_at.is_none());
    }
}

#[tokio::test]
async fn command_deadlines_and_output_limits_are_enforced() {
    let error = run_command("/bin/sleep", &["1".into()], Duration::from_millis(20))
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("timed out"));
    let input = vec![b'x'; MAX_OUTPUT as usize + 1];
    assert!(read_output(input.as_slice()).await.is_err());
}

#[tokio::test]
async fn failed_smart_does_not_invalidate_discovery_or_topology() {
    let runner = FixtureRunner::new();
    let first = observe(
        runner.clone(),
        "tank",
        StorageInventory::default(),
        100,
        SMART_BATCH,
    )
    .await;
    runner.0.lock().unwrap().failures = vec!["smartctl"];
    let result = observe(runner.clone(), "tank", first, 1000, SMART_BATCH).await;
    assert_eq!(result.disk_collection.observed_at, Some(1000));
    assert_eq!(result.pool_collection.observed_at, Some(1000));
    assert!(result.disk_collection.error.is_none());
    assert!(result.pool_collection.error.is_none());
    for disk in result.disks {
        assert_eq!(disk.smart.state, SmartState::Unavailable);
        assert_eq!(disk.smart.collection.observed_at, Some(100));
        assert_eq!(disk.smart.collection.attempted_at, Some(1000));
        assert_eq!(disk.smart.temperature_celsius, Some(32));
    }
}

#[test]
fn persisted_observations_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    save_cache(state.path(), &StorageInventory::default()).unwrap();
    let mode = std::fs::metadata(state.path().join("storage.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn cache_write_failure_keeps_live_observations_and_reports_the_failure() {
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir(state.path().join("storage.json")).unwrap();
    let inventory = collect_cached(FixtureRunner::new(), state.path(), "tank", 100)
        .await
        .unwrap();
    assert_eq!(inventory.disk_collection.observed_at, Some(100));
    assert_eq!(inventory.pool_collection.observed_at, Some(100));
    assert!(
        inventory
            .warnings
            .iter()
            .any(|warning| warning.contains("could not be persisted"))
    );
    assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn symlinked_cache_lock_is_rejected_before_probing() {
    let state = tempfile::tempdir().unwrap();
    let target = state.path().join("not-a-lock");
    std::fs::write(&target, b"unchanged").unwrap();
    std::os::unix::fs::symlink(&target, state.path().join("storage.lock")).unwrap();
    let runner = FixtureRunner::new();
    assert!(
        collect_cached(runner.clone(), state.path(), "tank", 100)
            .await
            .is_err()
    );
    assert!(runner.0.lock().unwrap().calls.is_empty());
    assert_eq!(std::fs::read(target).unwrap(), b"unchanged");
}
