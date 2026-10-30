use super::*;
use iced::widget::{column, row};

fn observation_notice(value: &str, error: bool) -> Element<'_, Msg> {
    container(typography(value, Type::BodyMedium))
        .padding(16)
        .width(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            background: Some(
                if error {
                    theme.colors.error_container
                } else {
                    theme.colors.secondary_container
                }
                .into(),
            ),
            text_color: Some(if error {
                theme.colors.on_error_container
            } else {
                theme.colors.on_secondary_container
            }),
            border: Border {
                radius: 16.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

fn age(observed: Option<u64>, now: u64) -> String {
    match observed {
        None => "not yet observed".into(),
        Some(observed) if observed > now => "timestamp is ahead of this device".into(),
        Some(observed) => match now.saturating_sub(observed) {
            0..60 => "less than 1 min ago".into(),
            seconds @ 60..3600 => format!("{} min ago", seconds / 60),
            _ => "more than an hour ago".into(),
        },
    }
}

fn collection<'a>(name: &str, status: &'a StorageCollection, now: u64) -> Element<'a, Msg> {
    let mut content = column![copy(format!(
        "{name}: {} · last success {}",
        if status.stale(now, 180) {
            "not current"
        } else {
            "current"
        },
        age(status.observed_at, now)
    ))]
    .spacing(6);
    if let Some(error) = &status.error {
        content = content.push(observation_notice(error, true));
    }
    content.into()
}

fn health(disk: &PhysicalDisk, inventory: &StorageInventory, now: u64) -> (&'static str, bool) {
    if inventory.disk_collection.stale(now, 180) {
        return ("SMART not current", false);
    }
    match disk.smart.state {
        SmartState::Pending => ("SMART pending", false),
        SmartState::Sleeping => ("SMART skipped: low power", false),
        SmartState::Unsupported => ("SMART unsupported", false),
        SmartState::Unavailable => ("SMART unavailable", false),
        SmartState::Current if disk.smart.collection.stale(now, 1800) => {
            ("SMART not current", false)
        }
        SmartState::Current => match disk.smart.passed {
            Some(true) => ("SMART passed", true),
            Some(false) => ("SMART reports failure", false),
            None => ("SMART health not reported", false),
        },
    }
}

fn memberships(inventory: &StorageInventory, id: &str) -> Vec<String> {
    fn visit(vdev: &StorageVdev, id: &str, pool: &str, found: &mut Vec<String>) {
        if vdev.disk_id.as_deref() == Some(id) {
            let membership = format!("{pool} · {} · {}", vdev.role, vdev.name);
            if !found.contains(&membership) {
                found.push(membership);
            }
        }
        for child in &vdev.children {
            visit(child, id, pool, found);
        }
    }
    let mut found = vec![];
    for pool in &inventory.pools {
        for vdev in &pool.vdevs {
            visit(vdev, id, &pool.name, &mut found);
        }
    }
    found
}

fn details<'a>(
    disk: &'a PhysicalDisk,
    inventory: &'a StorageInventory,
    now: u64,
) -> Element<'a, Msg> {
    let smart = &disk.smart;
    let mut content = column![
        typography("Drive identity & SMART", Type::TitleMedium),
        copy(format!(
            "Serial: {}",
            disk.serial.as_deref().unwrap_or("not reported")
        )),
        copy(format!(
            "WWN: {}",
            disk.wwn.as_deref().unwrap_or("not reported")
        )),
        copy(format!(
            "Transport: {} · identity {}",
            disk.transport.as_deref().unwrap_or("unknown"),
            disk.id
        )),
        copy(format!(
            "SMART last success {}",
            age(smart.collection.observed_at, now)
        )),
    ]
    .spacing(8);
    if let Some(error) = &smart.collection.error {
        content = content.push(observation_notice(error, true));
    }
    if let Some(passed) = smart.passed {
        content = content.push(copy(format!(
            "Last device-reported health: {}",
            if passed { "passed" } else { "FAILED" }
        )));
    }
    if let Some(temperature) = smart.temperature_celsius {
        content = content.push(copy(format!("Temperature: {temperature} °C")));
    }
    if let Some(hours) = smart.power_on_hours {
        content = content.push(copy(format!("Power-on time: {hours} hours")));
    }
    for metric in &smart.metrics {
        content = content.push(copy(format!("{}: {}", metric.name, metric.value)));
    }
    content = content.push(copy("Raw SMART attributes are vendor-specific. A passing SMART result is not a guarantee of disk reliability."));
    if smart.self_tests.is_empty() {
        content = content.push(copy(
            "No self-test history reported. No test is started by Homecloud.",
        ));
    } else {
        content = content.push(typography("Reported self-tests", Type::TitleSmall));
        for test in &smart.self_tests {
            content = content.push(copy(test));
        }
    }
    for device in &disk.devices {
        content = content.push(copy(format!(
            "{} · {} · {}",
            device.path,
            device
                .filesystem
                .as_deref()
                .unwrap_or("filesystem not reported"),
            if device.mountpoints.is_empty() {
                "no mountpoint reported".into()
            } else {
                device.mountpoints.join(", ")
            }
        )));
    }
    let uses = memberships(inventory, &disk.id);
    if uses.is_empty() {
        content = content.push(copy(if inventory.pool_collection.stale(now, 180) {
            "Pool membership is unknown or stale."
        } else {
            "Not matched to an imported pool. This does not mean the disk is unused."
        }));
    } else {
        for membership in uses {
            content = content.push(copy(format!("Pool membership: {membership}")));
        }
    }
    surface(content, true).into()
}

fn inspect<'a>(app: &App, disk: &PhysicalDisk) -> Element<'a, Msg> {
    button(format!(
        "{} {}",
        if app.storage_disk.as_ref() == Some(&disk.id) {
            "Hide"
        } else {
            "Inspect"
        },
        disk.path
    ))
    .variant(Variant::Text)
    .on_press(Msg::StorageDisk(disk.id.clone()))
    .into()
}

fn vdev<'a>(
    app: &'a App,
    node: &'a StorageVdev,
    inventory: &'a StorageInventory,
    now: u64,
    depth: usize,
) -> Element<'a, Msg> {
    let mut content = column![
        row![
            typography(
                format!("{} · {} · {}", node.name, node.kind, node.role),
                Type::TitleSmall
            ),
            horizontal_space(),
            badge(
                &node.state,
                node.state == "ONLINE" && !inventory.pool_collection.stale(now, 180)
            )
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    ]
    .spacing(8);
    if node.children.is_empty() {
        let count = |value: Option<u64>| {
            value
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".into())
        };
        content = content.push(copy(format!(
            "ZFS errors: read {} · write {} · checksum {}",
            count(node.read_errors),
            count(node.write_errors),
            count(node.checksum_errors)
        )));
        if let Some(path) = &node.path {
            content = content.push(copy(path));
        }
        content = content.push(copy(format!(
            "Vdev GUID: {}",
            if node.guid.is_empty() {
                "not reported"
            } else {
                &node.guid
            }
        )));
        if let Some(disk) = node
            .disk_id
            .as_ref()
            .and_then(|id| inventory.disks.iter().find(|disk| &disk.id == id))
        {
            content = content
                .push(copy(format!(
                    "{} · serial {}",
                    disk.model.as_deref().unwrap_or("Disk model not reported"),
                    disk.serial.as_deref().unwrap_or("not reported")
                )))
                .push(inspect(app, disk));
            if app.storage_disk.as_ref() == Some(&disk.id) {
                content = content.push(details(disk, inventory, now));
            }
        } else {
            content = content.push(copy(if node.kind == "file" {
                "File-backed vdev; no physical-disk SMART mapping."
            } else {
                "Physical disk not matched. Missing, virtual, or ambiguous devices remain visible."
            }));
        }
    }
    for child in &node.children {
        content = content.push(vdev(app, child, inventory, now, depth + 1));
    }
    container(content)
        .padding(iced::Padding {
            left: if depth == 0 { 0.0 } else { 12.0 },
            top: 6.0,
            right: 0.0,
            bottom: 6.0,
        })
        .into()
}

pub(super) fn view<'a>(app: &'a App, snapshot: &'a Snapshot) -> Element<'a, Msg> {
    if snapshot.me.role != Role::Admin {
        return observation_notice("Storage inventory is available to administrators.", false);
    }
    let Some(inventory) = &snapshot.inventory.storage else {
        return observation_notice(
            "Storage observations are not available from this controller. Upgrade the host software to enable read-only disk, topology and SMART collection.",
            false,
        );
    };
    let now = unix_now();
    let mut content = column![
        heading("Storage observability"),
        copy("Read-only observations of kernel-visible disks and imported ZFS pools. No formatting, repair, or self-tests are performed."),
        collection("Disk inventory", &inventory.disk_collection, now),
        collection("Pool topology", &inventory.pool_collection, now),
    ].spacing(16);
    for warning in &inventory.warnings {
        content = content.push(observation_notice(warning, true));
    }
    content = content.push(heading("Imported ZFS pools"));
    if inventory.pools.is_empty() {
        content = content.push(copy(if inventory.pool_collection.stale(now, 180) {
            "Pool topology has not been observed successfully. This is not evidence that no pools exist."
        } else { "No imported pools reported." }));
    }
    for pool in &inventory.pools {
        let mut pool_content = column![
            row![
                typography(&pool.name, Type::TitleMedium),
                horizontal_space(),
                badge(
                    &pool.state,
                    pool.state == "ONLINE" && !inventory.pool_collection.stale(now, 180)
                )
            ]
            .spacing(12),
            copy(if pool.managed {
                "Homecloud backing pool"
            } else {
                "Other imported pool"
            }),
        ]
        .spacing(10);
        for warning in &pool.warnings {
            pool_content = pool_content.push(observation_notice(warning, true));
        }
        if let Some(status) = &pool.status {
            pool_content = pool_content.push(copy(status));
        }
        pool_content = pool_content.push(copy(format!(
            "Scan: {}",
            pool.scan.as_deref().unwrap_or("not reported")
        )));
        for node in &pool.vdevs {
            pool_content = pool_content.push(vdev(app, node, inventory, now, 0));
        }
        content = content.push(surface(pool_content, false));
    }
    content = content.push(heading("All disks"))
        .push(copy("Includes OS disks and disks outside Homecloud's pool. Hardware hidden behind a controller may not be individually visible. NVMe health can apply to a controller shared by multiple namespaces."));
    if inventory.disks.is_empty() {
        content = content.push(copy(if inventory.disk_collection.stale(now, 180) {
            "Disk inventory has not been observed successfully."
        } else {
            "No kernel-visible physical disks reported."
        }));
    }
    for disk in &inventory.disks {
        let (status, good) = health(disk, inventory, now);
        let size = disk
            .size_bytes
            .map(|size| ResourceKind::Storage.format(size as u128))
            .unwrap_or_else(|| "capacity unknown".into());
        let mut disk_content = column![
            row![
                typography(
                    disk.model.as_deref().unwrap_or("Disk model not reported"),
                    Type::TitleMedium
                ),
                horizontal_space(),
                badge(status, good)
            ]
            .spacing(12),
            copy(format!(
                "{} · {} · {}",
                disk.path,
                size,
                match disk.rotational {
                    Some(true) => "rotational",
                    Some(false) => "non-rotational",
                    None => "media type unknown",
                }
            )),
            copy(format!(
                "Serial: {}",
                disk.serial.as_deref().unwrap_or("not reported")
            )),
        ]
        .spacing(10);
        if disk.devices.iter().any(|device| {
            device
                .mountpoints
                .iter()
                .any(|mount| matches!(mount.as_str(), "/" | "/boot" | "/boot/efi"))
        }) {
            disk_content = disk_content.push(copy("OS / boot filesystem reported on this disk."));
        }
        if memberships(inventory, &disk.id).is_empty() {
            disk_content = disk_content.push(copy(if inventory.pool_collection.stale(now, 180) {
                "Pool membership unknown / stale"
            } else {
                "Outside imported pools / membership not matched"
            }));
        }
        disk_content = disk_content.push(inspect(app, disk));
        if app.storage_disk.as_ref() == Some(&disk.id) {
            disk_content = disk_content.push(details(disk, inventory, now));
        }
        content = content.push(surface(disk_content, false));
    }
    content.into()
}
