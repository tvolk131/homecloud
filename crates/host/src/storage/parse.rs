use anyhow::{Result, bail, ensure};
use homecloud_core::*;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(super) fn text(value: &Value) -> Option<String> {
    let value = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        _ => return None,
    };
    let value: String = value.trim().chars().take(512).collect();
    (!value.is_empty()).then_some(value)
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn entries(value: &Value) -> Vec<(String, &Value)> {
    match value {
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| (key.clone(), value))
            .collect(),
        Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| (index.to_string(), value))
            .collect(),
        _ => Vec::new(),
    }
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn descendants<'node>(
    node: &'node Value,
    result: &mut Vec<&'node Value>,
    depth: usize,
) -> Result<()> {
    ensure!(depth <= 64, "Block-device topology exceeds nesting limit");
    result.push(node);
    for child in array(&node["children"]) {
        descendants(child, result, depth + 1)?;
    }
    Ok(())
}

fn device_path(node: &Value) -> Option<String> {
    text(&node["path"]).or_else(|| text(&node["name"]))
}

fn canonical(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned())
}

pub(super) fn disks(payload: &Value) -> Result<Vec<PhysicalDisk>> {
    let Some(roots) = payload["blockdevices"].as_array() else {
        bail!("lsblk did not report a blockdevices array");
    };
    let mut disks = Vec::new();
    let mut paths = BTreeSet::new();
    let mut identities = BTreeSet::new();
    for root in roots {
        let mut nodes = Vec::new();
        descendants(root, &mut nodes, 0)?;
        for node in nodes {
            if node["type"] != "disk" {
                continue;
            }
            let path = device_path(node).unwrap_or_default();
            ensure!(
                path.starts_with("/dev/")
                    && !Path::new(&path)
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir)),
                "lsblk reported a disk without a valid /dev path"
            );
            if !paths.insert(path.clone()) {
                continue;
            }
            let model = text(&node["model"]);
            let serial = text(&node["serial"]);
            let wwn = text(&node["wwn"]);
            let mut identity = if let Some(wwn) = &wwn {
                format!("wwn:{wwn}")
            } else if let Some(serial) = &serial {
                format!("serial:{}:{serial}", model.as_deref().unwrap_or_default())
            } else {
                format!("path:{path}")
            };
            if !identities.insert(identity.clone()) {
                identity.push_str(&format!(":{path}"));
                identities.insert(identity.clone());
            }
            let mut nodes = Vec::new();
            descendants(node, &mut nodes, 0)?;
            let devices = nodes
                .into_iter()
                .filter_map(|node| {
                    Some(StorageBlockDevice {
                        path: device_path(node)?,
                        filesystem: text(&node["fstype"]),
                        mountpoints: array(&node["mountpoints"])
                            .iter()
                            .filter_map(text)
                            .collect(),
                    })
                })
                .collect();
            let rotational = if node["rota"].is_null() {
                None
            } else {
                Some(matches!(text(&node["rota"]).as_deref(), Some("1" | "true")))
            };
            disks.push(PhysicalDisk {
                id: identity,
                path,
                model,
                serial,
                wwn,
                devices,
                rotational,
                transport: text(&node["tran"]),
                size_bytes: number(&node["size"]),
                smart: SmartObservation::default(),
            });
        }
    }
    Ok(disks)
}

fn vdev(
    key: &str,
    node: &Value,
    mapping: &BTreeMap<String, BTreeSet<String>>,
    inherited_role: &str,
    depth: usize,
) -> Result<StorageVdev> {
    ensure!(
        node.is_object() && depth <= 64,
        "ZFS returned an invalid or excessively nested vdev"
    );
    let name = text(&node["name"]).unwrap_or_else(|| key.to_owned());
    let role = text(&node["class"])
        .or_else(|| text(&node["alloc_bias"]))
        .unwrap_or_else(|| inherited_role.to_owned());
    let role = if node["is_log"].as_bool() == Some(true) || number(&node["is_log"]) == Some(1) {
        "log"
    } else {
        match role.as_str() {
            "normal" => "data",
            "l2cache" => "cache",
            "logs" => "log",
            other => other,
        }
    }
    .to_owned();
    let path = text(&node["path"])
        .or_else(|| text(&node["was"]))
        .or_else(|| name.starts_with('/').then(|| name.clone()));
    let disk_id = path
        .as_deref()
        .and_then(|path| mapping.get(&canonical(path)))
        .filter(|matches| matches.len() == 1)
        .and_then(|matches| matches.first().cloned());
    let missing = node["not_present"].as_bool() == Some(true)
        || number(&node["not_present"]).is_some_and(|value| value != 0)
        || text(&node["was"]).is_some();
    let mut children = Vec::new();
    for (field, child_role) in [
        ("vdevs", role.as_str()),
        ("l2cache", "cache"),
        ("spares", "spare"),
        ("special", "special"),
        ("dedup", "dedup"),
        ("logs", "log"),
    ] {
        for (key, child) in entries(&node[field]) {
            children.push(vdev(&key, child, mapping, child_role, depth + 1)?);
        }
    }
    Ok(StorageVdev {
        guid: text(&node["guid"]).unwrap_or_default(),
        name,
        kind: text(&node["vdev_type"])
            .or_else(|| text(&node["type"]))
            .unwrap_or_else(|| "unknown".into()),
        role,
        state: text(&node["state"]).unwrap_or_else(|| "unknown".into()),
        path,
        disk_id: if missing { None } else { disk_id },
        read_errors: number(&node["read_errors"]),
        write_errors: number(&node["write_errors"]),
        checksum_errors: number(&node["checksum_errors"]),
        children,
    })
}

fn redundancy_warnings(vdevs: &[StorageVdev], warnings: &mut Vec<String>) {
    for vdev in vdevs {
        if vdev.kind == "root" {
            redundancy_warnings(&vdev.children, warnings);
        } else if matches!(vdev.role.as_str(), "data" | "special" | "dedup")
            && (matches!(vdev.kind.as_str(), "disk" | "file")
                || vdev.kind == "mirror" && vdev.children.len() < 2)
        {
            warnings.push(format!("No redundancy in {} vdev {}", vdev.role, vdev.name));
        }
    }
}

pub(super) fn pools(
    payload: &Value,
    disks: &[PhysicalDisk],
    managed_pool: &str,
) -> Result<Vec<StoragePool>> {
    ensure!(
        payload["pools"].is_object() || payload["pools"].is_array(),
        "Structured ZFS status unavailable; OpenZFS 2.3+ JSON support is required"
    );
    let mut mapping: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for disk in disks {
        for device in &disk.devices {
            mapping
                .entry(canonical(&device.path))
                .or_default()
                .insert(disk.id.clone());
        }
    }
    let mut pools = Vec::new();
    for (key, pool) in entries(&payload["pools"]) {
        ensure!(pool.is_object(), "ZFS returned an invalid pool");
        let mut vdevs = Vec::new();
        for (field, role) in [
            ("vdevs", "data"),
            ("l2cache", "cache"),
            ("spares", "spare"),
            ("special", "special"),
            ("dedup", "dedup"),
            ("logs", "log"),
        ] {
            for (key, node) in entries(&pool[field]) {
                vdevs.push(vdev(&key, node, &mapping, role, 0)?);
            }
        }
        ensure!(!vdevs.is_empty(), "ZFS pool topology was not reported");
        let mut warnings = Vec::new();
        redundancy_warnings(&vdevs, &mut warnings);
        let scan = pool
            .get("scan_stats")
            .filter(|value| value.is_object())
            .unwrap_or(&pool["scan"]);
        let scan = [text(&scan["function"]), text(&scan["state"])]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let name = text(&pool["name"]).unwrap_or(key);
        pools.push(StoragePool {
            managed: name == managed_pool,
            name,
            guid: text(&pool["pool_guid"])
                .or_else(|| text(&pool["guid"]))
                .unwrap_or_default(),
            state: text(&pool["state"]).unwrap_or_else(|| "unknown".into()),
            status: text(&pool["status"]),
            scan: (!scan.is_empty()).then_some(scan),
            vdevs,
            warnings,
        });
    }
    Ok(pools)
}

pub(super) fn smart_failure(
    mut previous: SmartObservation,
    now: u64,
    state: SmartState,
    error: impl Into<String>,
) -> SmartObservation {
    previous.state = state;
    previous.collection.attempted_at = Some(now);
    previous.collection.error = Some(error.into().chars().take(512).collect());
    previous
}

pub(super) fn skipped_smart(
    code: i32,
    previous: SmartObservation,
    now: u64,
) -> Option<SmartObservation> {
    match code {
        99 => Some(smart_failure(
            previous,
            now,
            SmartState::Sleeping,
            "Low-power device; SMART probe skipped",
        )),
        98 => Some(smart_failure(
            previous,
            now,
            SmartState::Unsupported,
            "Power-mode check unsupported; SMART probe skipped to avoid waking the disk",
        )),
        _ => None,
    }
}

pub(super) fn smart(
    payload: &Value,
    code: i32,
    now: u64,
    previous: SmartObservation,
) -> SmartObservation {
    let error = array(&payload["smartctl"]["messages"])
        .iter()
        .filter_map(|message| text(&message["string"]))
        .collect::<Vec<_>>()
        .join("; ");
    if code < 0 || code & 3 != 0 {
        return smart_failure(
            previous,
            now,
            SmartState::Unavailable,
            if error.is_empty() {
                "SMART device could not be opened"
            } else {
                &error
            },
        );
    }
    if payload["smart_support"]["available"] == false {
        return smart_failure(
            previous,
            now,
            SmartState::Unsupported,
            "Device does not report SMART support",
        );
    }
    let passed = payload["smart_status"]["passed"].as_bool();
    let temperature_celsius = payload["temperature"]["current"]
        .as_i64()
        .or_else(|| payload["temperature"]["current"].as_str()?.parse().ok());
    let power_on_hours = number(&payload["power_on_time"]["hours"]);
    let attributes = array(&payload["ata_smart_attributes"]["table"]);
    let nvme = payload["nvme_smart_health_information_log"].as_object();
    if passed.is_none()
        && temperature_celsius.is_none()
        && power_on_hours.is_none()
        && attributes.is_empty()
        && nvme.is_none_or(|values| values.is_empty())
    {
        return smart_failure(
            previous,
            now,
            SmartState::Unavailable,
            if error.is_empty() {
                "No supported SMART health data returned"
            } else {
                &error
            },
        );
    }
    let mut metrics = Vec::new();
    for attribute in attributes {
        if !matches!(
            number(&attribute["id"]),
            Some(5 | 187 | 188 | 197 | 198 | 199)
        ) {
            continue;
        }
        if let Some(value) =
            text(&attribute["raw"]["string"]).or_else(|| text(&attribute["raw"]["value"]))
        {
            metrics.push(SmartMetric {
                name: format!(
                    "{} (raw)",
                    text(&attribute["name"])
                        .or_else(|| text(&attribute["id"]))
                        .unwrap_or_default()
                ),
                value: value.chars().take(128).collect(),
            });
        }
    }
    if let Some(nvme) = nvme {
        for (field, name) in [
            ("critical_warning", "Critical warning flags"),
            ("available_spare", "Available spare (%)"),
            ("available_spare_threshold", "Spare threshold (%)"),
            ("percentage_used", "Percentage used"),
            ("media_errors", "Media / data integrity errors"),
            ("num_err_log_entries", "Error log entries"),
            ("unsafe_shutdowns", "Unsafe shutdowns"),
            ("data_units_read", "Data units read"),
            ("data_units_written", "Data units written"),
        ] {
            if let Some(value) = nvme.get(field).and_then(text) {
                metrics.push(SmartMetric {
                    name: name.into(),
                    value: value.chars().take(128).collect(),
                });
            }
        }
    }
    let mut self_tests = Vec::new();
    for section in ["extended", "standard"] {
        for entry in array(&payload["ata_smart_self_test_log"][section]["table"])
            .iter()
            .take(5)
        {
            self_tests.push(format!(
                "{}: {}",
                text(&entry["type"]["string"]).unwrap_or_else(|| "Self-test".into()),
                text(&entry["status"]["string"]).unwrap_or_else(|| "Status not reported".into())
            ));
        }
        if !self_tests.is_empty() {
            break;
        }
    }
    for entry in array(&payload["nvme_self_test_log"]["table"])
        .iter()
        .take(5)
    {
        self_tests.push(format!(
            "NVMe self-test: {}",
            text(&entry["self_test_result"]["string"])
                .unwrap_or_else(|| "Status not reported".into())
        ));
    }
    SmartObservation {
        collection: StorageCollection {
            attempted_at: Some(now),
            observed_at: Some(now),
            error: (code & 4 != 0).then(|| {
                if error.is_empty() {
                    "Some SMART commands failed".into()
                } else {
                    error.chars().take(512).collect()
                }
            }),
        },
        state: SmartState::Current,
        passed,
        temperature_celsius,
        power_on_hours,
        metrics,
        self_tests,
    }
}
