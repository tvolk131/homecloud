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
    assert_eq!(disks[2].id, "wwn:0x5000a:/dev/sdb");
    payload["blockdevices"][0]["path"] = json!("/dev/../tmp/invalid");
    assert!(parse::disks(&payload).is_err());
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
