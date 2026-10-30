use super::*;
use iced::{Settings, Size};
use iced_test::Simulator;
use std::{
    fs,
    path::Path,
    sync::{Mutex, OnceLock},
};

pub(super) fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
pub(super) fn fixture() -> App {
    let grant = Resources {
        memory_bytes: 16 * GIB,
        cpu_cores: 4,
        disk_bytes: 240 * GIB,
    };
    let me = User {
        id: id(1),
        name: "Tommy".into(),
        role: Role::Admin,
        active: true,
        grant,
        committed: Resources {
            memory_bytes: 4 * GIB,
            cpu_cores: 1,
            disk_bytes: 84 * GIB,
        },
        gpu_allowed: true,
    };
    let vm = |n, name: &str, state: &str| Vm {
        id: id(n),
        owner: me.id,
        spec: VmSpec {
            applications: vec![],
            name: name.into(),
            memory_gib: 4,
            cores: 1,
            disk_gib: 40,
            ssh_public_key: "ssh-ed25519 FIXTURE_PUBLIC_KEY".into(),
        },
        resources: Resources {
            memory_bytes: if state == "running" { 4 * GIB } else { 0 },
            cpu_cores: if state == "running" { 1 } else { 0 },
            disk_bytes: 42 * GIB,
        },
        cpu_ids: if state == "running" {
            vec![4, 10]
        } else {
            vec![]
        },
        network_slot: n as u16,
        desired: if state == "running" {
            Lifecycle::Running
        } else {
            Lifecycle::Stopped
        },
        observed: state.into(),
        ssh_host_key: Some("ssh-ed25519 FIXTURE_HOST_KEY".into()),
        gpus: vec![],
        preferred_gpus: vec![],
        pending_gpus: vec![],
        error: None,
    };
    let vms = vec![
        vm(10, "T3 development", "running"),
        vm(11, "Experiments", "stopped"),
    ];
    let users = vec![
        me.clone(),
        User {
            id: id(2),
            name: "Alex Chen".into(),
            role: Role::User,
            active: true,
            grant: Resources {
                memory_bytes: 8 * GIB,
                cpu_cores: 2,
                disk_bytes: 80 * GIB,
            },
            committed: Resources::default(),
            gpu_allowed: false,
        },
    ];
    let inventory = HostInventory {
        storage: None,
        hostname: "homecloud-01".into(),
        cpu_model: "Intel Core i7-8700K".into(),
        physical_cores: 6,
        memory_bytes: 32 * GIB,
        pool: "tank".into(),
        pool_available_bytes: 620 * GIB,
        minimum_pool_available_bytes: Some(30 * GIB),
        pool_health: "ONLINE".into(),
        iommu: Some(Readiness {
            state: "ready".into(),
            evidence: vec!["Intel DMAR present · 12 IOMMU groups".into()],
            next_steps: vec![],
        }),
        gpus: vec![Gpu {
            pci: "0000:01:00.0".into(),
            model: "NVIDIA Corporation GP104 [GeForce GTX 1070 Ti]".into(),
            driver: Some("nouveau".into()),
            iommu_group: Some(1),
            host_display: true,
            readiness: Readiness {
                state: "ready".into(),
                evidence: vec!["GPU and audio function share an isolated IOMMU group.".into()],
                next_steps: vec![],
            },
            assigned_vm: None,
            telemetry: None,
        }],
        warnings: vec![],
    };
    let snapshot = Snapshot {
        memory_health: None,
        host_reserves: None,
        disk_overhead_gib_per_vm: 2,
        applications: vec![],
        application_operations: vec![],
        me: me.clone(),
        users,
        devices: vec![
            Device {
                peer: "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890".into(),
                user_id: id(1),
                active: true,
            },
            Device {
                peer: "9876543210abcdef9876543210abcdef9876543210abcdef9876543210abcdef".into(),
                user_id: id(2),
                active: true,
            },
        ],
        vms,
        operations: vec![Operation {
            progress: None,
            id: id(100),
            vm_id: id(10),
            actor: id(1),
            request: Mutation::Create {
                spec: vm(10, "T3 development", "running").spec,
            },
            phase: "succeeded".into(),
            error: None,
        }],
        inventory,
        capacity: grant,
    };
    let mut app = App::blank("fixture-only-do-not-connect.json".into());
    app.snapshot = Some(snapshot);
    app.reduced_motion = true;
    app.fields[Field::Key as usize] =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFixturePublicKeyOnly".into();
    app
}
pub(super) fn host_fixture() -> App {
    let mut app = fixture();
    app.page = Page::Host;
    let s = app.snapshot.as_mut().unwrap();
    s.capacity = Resources {
        memory_bytes: 24 * GIB,
        cpu_cores: 4,
        disk_bytes: 8192 * GIB,
    };
    s.me.grant = Resources {
        memory_bytes: 16 * GIB,
        cpu_cores: 3,
        disk_bytes: 2048 * GIB,
    };
    s.me.committed = Resources {
        memory_bytes: 8 * GIB,
        cpu_cores: 2,
        disk_bytes: 400 * GIB,
    };
    s.users[0] = s.me.clone();
    s.users[1].grant = Resources {
        memory_bytes: 4 * GIB,
        cpu_cores: 1,
        disk_bytes: 1024 * GIB,
    };
    s.users[1].committed = Resources {
        memory_bytes: 2 * GIB,
        cpu_cores: 1,
        disk_bytes: 80 * GIB,
    };
    s.vms[0].spec.memory_gib = 8;
    s.vms[0].spec.cores = 2;
    s.vms[0].spec.disk_gib = 356;
    s.vms[0].resources = Resources {
        memory_bytes: 8 * GIB,
        cpu_cores: 2,
        disk_bytes: 358 * GIB,
    };
    s.vms[0].cpu_ids = vec![4, 10, 5, 11];
    s.vms[0].gpus = vec!["0000:01:00.0".into()];
    s.vms[0].preferred_gpus = s.vms[0].gpus.clone();
    let mut alex = s.vms[0].clone();
    alex.id = id(12);
    alex.owner = id(2);
    alex.spec.name = "Alex's workspace".into();
    alex.spec.memory_gib = 2;
    alex.spec.cores = 1;
    alex.spec.disk_gib = 78;
    alex.resources = s.users[1].committed;
    alex.cpu_ids = vec![2, 8];
    alex.gpus = vec![];
    alex.preferred_gpus = vec![];
    s.vms.push(alex);
    s.inventory.pool_available_bytes = 6144 * GIB;
    app
}
pub(super) fn storage_fixture() -> App {
    let mut app = host_fixture();
    app.host_tab = HostTab::Storage;
    let mut inventory: StorageInventory = serde_json::from_value(serde_json::json!({
        "disks": [
            {"id": "wwn:fixture-hdd", "path": "/dev/sda", "model": "Fixture storage HDD",
             "serial": "SYNTHETIC-STORAGE", "wwn": "fixture-hdd", "transport": "sata",
             "size_bytes": 1000000000000u64, "rotational": true,
             "devices": [{"path": "/dev/sda1", "filesystem": "zfs_member"}],
             "smart": {"state": "current", "passed": true, "temperature_celsius": 32,
                "power_on_hours": 1200,
                "metrics": [{"name": "Current_Pending_Sector (raw)", "value": "0"}],
                "self_tests": ["Short offline: Completed without error"]}},
            {"id": "serial:fixture-boot", "path": "/dev/nvme0n1", "model": "Fixture OS NVMe",
             "serial": "SYNTHETIC-BOOT", "transport": "nvme", "size_bytes": 500000000000u64,
             "rotational": false,
             "devices": [{"path": "/dev/nvme0n1p1", "filesystem": "ext4", "mountpoints": ["/"]}],
             "smart": {"state": "current", "passed": false, "temperature_celsius": 45,
                "metrics": [{"name": "Critical warning flags", "value": "4"},
                            {"name": "Percentage used", "value": "104"},
                            {"name": "Media / data integrity errors", "value": "2"}]}}
        ],
        "pools": [{"name": "tank", "guid": "100", "state": "ONLINE", "managed": true,
            "scan": "scrub · finished", "warnings": ["No redundancy in data vdev /dev/sda1"],
            "vdevs": [{"name": "/dev/sda1", "guid": "101", "kind": "disk", "role": "data",
                "path": "/dev/sda1", "state": "ONLINE", "disk_id": "wwn:fixture-hdd",
                "read_errors": 0, "write_errors": 0, "checksum_errors": 2}]}]
    }))
    .unwrap();
    let now = unix_now();
    inventory.disk_collection.observed_at = Some(now);
    inventory.pool_collection.observed_at = Some(now);
    for disk in &mut inventory.disks {
        disk.smart.collection.observed_at = Some(now);
    }
    app.snapshot.as_mut().unwrap().inventory.storage = Some(inventory);
    app
}
pub(super) fn simulator(app: &App, size: (f32, f32)) -> Simulator<'_, Msg, Theme> {
    Simulator::with_size(
        Settings {
            default_font: iced_m3::fonts::REGULAR,
            fonts: vec![iced_m3::fonts::ROBOTO.into()],
            ..Default::default()
        },
        Size::from(size),
        app.view(),
    )
}
fn lifecycle_id(vm: Uuid) -> iced::widget::Id {
    iced::widget::Id::from(format!("vm-lifecycle-{vm}"))
}
pub(super) fn scroll_bottom(ui: &mut Simulator<'_, Msg, Theme>, size: (f32, f32)) {
    ui.point_at((size.0 - 100.0, size.1 - 100.0));
    ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
        delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -3000.0 },
    })]);
}
pub(super) fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[test]
fn material_symbols_redistribution_notices_match_the_catalog() {
    assert_eq!(
        include_str!("../assets/material/LICENSE"),
        iced_m3::material_symbols::LICENSE
    );
    assert_eq!(
        include_str!("../assets/material/NOTICE"),
        iced_m3::material_symbols::NOTICE
    );
}

#[test]
fn screenshot_regressions() {
    // Pin the pixel backend even for a plain `cargo test --workspace`. Setting
    // process-wide environment variables inside parallel tests would be unsafe.
    if std::env::var("ICED_TEST_BACKEND").as_deref() != Ok("tiny-skia") {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env("ICED_TEST_BACKEND", "tiny-skia")
            .args([
                "--exact",
                "visual_tests::screenshot_regressions",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _guard = lock();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let baseline = root.join("tests/screenshots");
    let artifacts = root.join("../../target/ui-screenshots");
    let update = std::env::var("UPDATE_SCREENSHOTS").as_deref() == Ok("1");
    let names = [
        "host-storage-topology",
        "host-storage-smart",
        "host-storage-disks-small",
        "host-storage-stale",
        "host-storage-unavailable-small",
        "memory-vms-blocked-small",
        "memory-vm-blocked",
        "memory-host-recovery",
        "memory-member-blocked",
        "memory-promises-only",
        "memory-arc-blocked",
        "memory-arc-repair",
        "managed-host-create",
        "managed-host-details",
        "host-memory-growth",
        "host-memory-deficit",
        "host-memory-review",
        "setup-progress",
        "setup-review",
        "setup-review-small",
        "setup-logs",
        "setup-logs-small",
        "workspaces",
        "small-window",
        "workspaces-fab-bottom-small",
        "workspace-fab-bottom-small",
        "workspace-fab-disabled-small",
        "empty-small",
        "wide-window",
        "workspaces-ready",
        "workspaces-attention",
        "workspaces-cleanup",
        "workspaces-updating",
        "workspaces-starting",
        "workspaces-unavailable",
        "workspace-stop-timeout",
        "force-stop-confirmation",
        "force-stop-confirmation-small",
        "lifecycle-tooltip",
        "stop-confirmation",
        "stop-confirmation-small",
        "workspace-accounts",
        "workspace-accounts-small",
        "workspace-edit-gpu-small",
        "workspace-edit-multi-gpu",
        "workspace-edit-multi-gpu-small",
        "workspace-edit-low-memory",
        "workspace-edit-low-memory-small",
        "workspace-edit-rename",
        "workspace-edit-restart",
        "workspace-edit-restart-small",
        "workspace-edit-stopped-small",
        "workspace-edit-confirmation",
        "workspace-edit-confirmation-small",
        "workspace-name-hover",
        "member",
        "empty",
        "over-quota",
        "host",
        "host-memory-policy",
        "host-memory-policy-small",
        "host-memory-policy-cache-small",
        "host-memory-policy-automatic",
        "host-memory-policy-status",
        "host-reserves",
        "host-reserves-small",
        "host-reserves-invalid-small",
        "host-reserves-no-headroom-small",
        "host-reserves-pending",
        "host-reserves-review",
        "host-reserves-unapplied",
        "hosts-directory",
        "hosts-directory-small",
        "hosts-directory-member",
        "host-headroom",
        "host-headroom-missing",
        "host-allocations-small",
        "host-allocations-expanded-small",
        "hardware-setup",
        "host-small",
        "host-selected",
        "host-allocations",
        "host-storage",
        "host-over-quota",
        "host-member",
        "host-gpu-releasing",
        "host-hardware",
        "host-large-cpu",
        "host-zero-capacity",
        "settings",
        "people",
        "activity",
        "create",
        "create-small",
        "resource-grant",
        "resource-grant-small",
        "resource-invite-small",
        "resource-edit-over-quota-small",
        "resource-create-no-capacity-small",
        "workspace-accounts-long-small",
        "resize",
        "workspace-settings",
        "workspace-settings-small",
        "workspace-gpu-preference",
        "workspace-settings-lower-small",
        "workspace-stopped",
        "workspace-cleanup",
        "workspace-ready",
        "workspace-ready-small",
        "workspace-long-name",
        "workspace-member-settings",
        "gpu-unavailable",
        "gpu-unavailable-small",
        "stopping",
        "t3-setup",
        "t3-waiting",
        "t3-interrupted",
        "t3-connected",
        "t3-stopped",
        "t3-providers-missing",
        "t3-providers-installed",
        "t3-codex-waiting",
        "t3-codex-waiting-small",
        "t3-codex-signed-in",
        "t3-codex-interrupted",
        "t3-codex-submitting",
        "t3-codex-queued",
        "t3-codex-verifying",
        "t3-claude-waiting",
        "t3-claude-waiting-small",
        "t3-claude-verifying",
        "t3-details",
        "t3-waiting-small",
        "delete",
        "pairing",
    ];
    let mut failures = vec![];
    for name in names {
        let mut app = if name.starts_with("host") || name == "hardware-setup" {
            host_fixture()
        } else {
            fixture()
        };
        let mut size = (1180.0, 820.0);
        match name {
            "host-storage-topology"
            | "host-storage-smart"
            | "host-storage-disks-small"
            | "host-storage-stale"
            | "host-storage-unavailable-small" => {
                app = storage_fixture();
                size = (1180.0, 1040.0);
                if name == "host-storage-smart" {
                    app.storage_disk = Some("wwn:fixture-hdd".into());
                }
                if name == "host-storage-disks-small" {
                    size = (800.0, 600.0);
                    app.storage_disk = Some("serial:fixture-boot".into());
                }
                if name == "host-storage-stale" {
                    let inventory = app
                        .snapshot
                        .as_mut()
                        .unwrap()
                        .inventory
                        .storage
                        .as_mut()
                        .unwrap();
                    inventory.disk_collection.error =
                        Some("Disk discovery timed out; previous readings retained.".into());
                    inventory.pool_collection.error =
                        Some("ZFS observation failed; previous topology retained.".into());
                }
                if name == "host-storage-unavailable-small" {
                    size = (800.0, 600.0);
                    app.snapshot.as_mut().unwrap().inventory.storage =
                        Some(StorageInventory::default());
                }
            }
            "memory-vms-blocked-small"
            | "memory-vm-blocked"
            | "memory-host-recovery"
            | "memory-member-blocked"
            | "memory-promises-only"
            | "memory-arc-blocked"
            | "memory-arc-repair" => {
                let condition = match name {
                    "memory-promises-only" => MemoryCondition::PromisesExceedCapacity,
                    "memory-arc-blocked" | "memory-arc-repair" => MemoryCondition::ArcNotEnforced,
                    _ => MemoryCondition::AllocationsExceedCapacity,
                };
                app = crate::memory_ui_tests::fixture(condition);
                size = (1180.0, 1040.0);
                if name == "memory-arc-repair" {
                    let _ = app.update(Msg::Open(Panel::HostReserves));
                }
                if name == "memory-vms-blocked-small" {
                    size = (800.0, 600.0);
                }
                if name == "memory-vm-blocked" {
                    let _ = app.update(Msg::OpenWorkspace(id(10)));
                }
                if name == "memory-host-recovery" {
                    let _ = app.update(Msg::ReviewHostMemory);
                }
                if name == "memory-member-blocked" {
                    let s = app.snapshot.as_mut().unwrap();
                    s.me.role = Role::User;
                    s.users.retain(|u| u.id == s.me.id);
                    s.vms.retain(|v| v.owner == s.me.id);
                    s.memory_health.as_mut().unwrap().details = None;
                    size = (800.0, 600.0);
                }
                if name == "memory-promises-only" {
                    for vm in &mut app.snapshot.as_mut().unwrap().vms {
                        vm.observed = "running".into();
                    }
                }
            }
            "managed-host-create" => {
                let _ = app.update(Msg::Open(Panel::Create));
                let _ = app.update(Msg::ToggleManagedHost);
                size = (900.0, 900.0);
            }
            "managed-host-details" => {
                let s = app.snapshot.as_mut().unwrap();
                s.vms[0].spec.applications = vec![ApplicationKind::HomecloudHost];
                s.applications=vec![ManagedApplication{id:id(200),vm_id:s.vms[0].id,kind:ApplicationKind::HomecloudHost,desired_version:"0.1.0".into(),start_on_boot:true,observation:ApplicationObservation { installed_version:Some("0.1.0".into()),service:"running".into(),setup:"ready".into(),message:"Host is ready. Bootstrap its independent administrator from its terminal.".into(),components:vec![ApplicationComponent{name:"Homecloud".into(),version:Some("active".into()),ready:true,..Default::default()},ApplicationComponent{name:"Incus".into(),version:Some("active".into()),ready:true,..Default::default()},ApplicationComponent{name:"ZFS pool".into(),version:Some("ONLINE".into()),ready:true,..Default::default()}],..Default::default() }}];
                let _ = app.update(Msg::OpenWorkspace(id(10)));
            }
            "host-memory-growth" | "host-memory-deficit" | "host-memory-review" => {
                app = crate::reserve_ui_tests::memory_fixture();
                let s = app.snapshot.as_mut().unwrap();
                let view = s.host_reserves.as_mut().unwrap();
                view.memory = Some(MemoryCapacityStatus {
                    revision: 0,
                    reviewed_bytes: 28 * GIB,
                    safe_bytes: if name == "host-memory-deficit" {
                        4 * GIB
                    } else {
                        60 * GIB
                    },
                    physical_bytes: if name == "host-memory-deficit" {
                        8 * GIB
                    } else {
                        64 * GIB
                    },
                    minimum_system_bytes: 4 * GIB,
                    boot_id: id(5).to_string(),
                    observed_at: unix_now(),
                    arc_ready: true,
                    error: None,
                });
                if name == "host-memory-review" {
                    let _ = app.update(Msg::Open(Panel::ReviewMemoryCapacity));
                }
                size = (1180.0, 1040.0);
            }
            "setup-progress" | "setup-review" | "setup-review-small" | "setup-logs"
            | "setup-logs-small" => {
                app = setup_fixture(name != "setup-progress");
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                if name.starts_with("setup-logs") {
                    app.panel = Some(Panel::SetupDiagnostics(id(10)));
                    app.setup_diagnostics = Some((
                        id(10),
                        Ok(VmDiagnostics {
                            vm_id: id(10),
                            progress: Some(VmSetupProgress {
                                stage: VmSetupStage::GuestSetup,
                                since: 123,
                            }),
                            power_state: "running".into(),
                            checks: vec!["Incus and QEMU VSOCK configuration agree.".into()],
                            logs: vec![VmSetupLog {
                                name: "/var/log/cloud-init-output.log".into(),
                                text: "Cloud-init: package installation
Temporary failure resolving archive.ubuntu.com
Setup did not complete.
"
                                .repeat(10),
                            }],
                        }),
                    ));
                }
            }

            "host-memory-policy"
            | "host-memory-policy-small"
            | "host-memory-policy-cache-small"
            | "host-memory-policy-automatic"
            | "host-memory-policy-status" => {
                app = crate::reserve_ui_tests::memory_fixture();
                if name != "host-memory-policy-status" {
                    let _ = app.update(Msg::Open(Panel::HostReserves));
                }
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                if name == "host-memory-policy-automatic" {
                    let _ = app.update(Msg::ReserveAutomatic(true));
                }
            }
            "host-reserves"
            | "host-reserves-small"
            | "host-reserves-invalid-small"
            | "host-reserves-no-headroom-small" => {
                app = crate::reserve_ui_tests::fixture();
                if name.contains("no-headroom") {
                    let s = app.snapshot.as_mut().unwrap();
                    s.me.grant = s.capacity.checked_sub(s.users[1].grant).unwrap();
                    s.users[0] = s.me.clone();
                    s.host_reserves.as_mut().unwrap().protected_backing = s.capacity;
                }
                let _ = app.update(Msg::Open(Panel::HostReserves));
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                if name.contains("invalid") {
                    let _ = app.update(Msg::Edit(Field::ReserveMemory, "5".into()));
                }
            }
            "host-reserves-pending" | "host-reserves-review" | "host-reserves-unapplied" => {
                app = crate::reserve_ui_tests::fixture();
                size = (1180.0, 1040.0);
                let mut op = crate::reserve_ui_tests::operation();
                if name != "host-reserves-pending" {
                    op.phase = ReservePhase::NeedsReview;
                    op.error = Some(
                        if name == "host-reserves-unapplied" {
                            "Pool headroom changed before enforcement."
                        } else {
                            "CPU fence update was interrupted. Retry to complete."
                        }
                        .into(),
                    );
                }
                let s = app.snapshot.as_mut().unwrap();
                let view = s.host_reserves.as_mut().unwrap();
                s.capacity = view.envelope.checked_sub(op.target.additional).unwrap();
                view.operation = Some(op);
                view.can_discard = name == "host-reserves-unapplied";
            }

            "hosts-directory" | "hosts-directory-small" | "hosts-directory-member" => {
                app.page = Page::Hosts;
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                if name.ends_with("member") {
                    app.snapshot.as_mut().unwrap().me.role = Role::User;
                }
            }
            "host-headroom" | "host-headroom-missing" => {
                size = (1180.0, 1040.0);
                if name.ends_with("missing") {
                    app.snapshot
                        .as_mut()
                        .unwrap()
                        .inventory
                        .minimum_pool_available_bytes = None;
                }
            }
            "host-allocations-small" | "host-allocations-expanded-small" => {
                size = (800.0, 600.0);
                app.host_tab = HostTab::Allocations;
                if name.contains("expanded") {
                    app.host_owner = Some(id(2));
                }
            }
            "workspaces-fab-bottom-small" => {
                size = (800.0, 600.0);
                let s = app.snapshot.as_mut().unwrap();
                let vm = s.vms[0].clone();
                for n in 20..25 {
                    let mut extra = vm.clone();
                    extra.id = id(n);
                    extra.spec.name = format!("Project {}", n - 18);
                    s.vms.push(extra);
                }
            }
            "workspace-fab-bottom-small" | "workspace-fab-disabled-small" => {
                size = (800.0, 600.0);
                let _ = app.update(Msg::OpenWorkspace(id(10)));
                app.advanced = true;
                if name == "workspace-fab-disabled-small" {
                    app.snapshot.as_mut().unwrap().vms[0].desired = Lifecycle::Stopped;
                }
            }

            "resource-grant" | "resource-grant-small" => {
                app = host_fixture();
                let _ = app.update(Msg::Select(id(2)));
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
            }
            "resource-invite-small" => {
                app = host_fixture();
                let _ = app.update(Msg::Open(Panel::Invite));
                let _ = app.update(Msg::Edit(Field::User, "New coworker".into()));
                size = (800.0, 600.0);
            }
            "resource-edit-over-quota-small" => {
                let s = app.snapshot.as_mut().unwrap();
                s.me.grant.memory_bytes = 2 * GIB;
                s.me.grant.cpu_cores = 0;
                s.me.grant.disk_bytes = 32 * GIB;
                let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
                size = (800.0, 600.0);
            }
            "resource-create-no-capacity-small" => {
                let s = app.snapshot.as_mut().unwrap();
                s.me.grant = s.me.committed;
                let _ = app.update(Msg::Open(Panel::Create));
                size = (800.0, 600.0);
            }
            "workspace-accounts-long-small" => {
                app = account_fixture();
                app.snapshot.as_mut().unwrap().applications[0].observation.accounts[1].email=Some("a.deliberately.long.development.account.address.for.layout.coverage@example-development.test".into());
                size = (800.0, 600.0);
            }
            "workspace-edit-multi-gpu"
            | "workspace-edit-multi-gpu-small"
            | "workspace-edit-low-memory"
            | "workspace-edit-low-memory-small" => {
                add_second_gpu(&mut app);
                let _ = app.update(Msg::OpenWorkspace(id(10)));
                let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
                let _ = app.update(Msg::ToggleEditGpu("0000:01:00.0".into(), true));
                let _ = app.update(Msg::ToggleEditGpu("0000:02:00.0".into(), true));
                if name.contains("low-memory") {
                    let _ = app.update(Msg::Edit(Field::Ram, "1".into()));
                }
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
            }
            "workspace-edit-gpu-small" => {
                let vm = &mut app.snapshot.as_mut().unwrap().vms[0];
                vm.gpus = vec!["0000:01:00.0".into()];
                vm.preferred_gpus = vm.gpus.clone();
                let _ = app.update(Msg::OpenWorkspace(id(10)));
                let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
                size = (800.0, 600.0);
            }
            "workspace-edit-rename"
            | "workspace-edit-restart"
            | "workspace-edit-restart-small"
            | "workspace-edit-stopped-small"
            | "workspace-edit-confirmation"
            | "workspace-edit-confirmation-small" => {
                let selected = if name == "workspace-edit-stopped-small" {
                    id(11)
                } else {
                    id(10)
                };
                let _ = app.update(Msg::OpenWorkspace(selected));
                let _ = app.update(Msg::Open(Panel::EditWorkspace(selected)));
                if name == "workspace-edit-rename" {
                    app.fields[Field::Name as usize] = "Renamed development".into();
                } else {
                    app.fields[Field::Ram as usize] = "8".into();
                }
                if name.contains("confirmation") {
                    let _ = app.update(Msg::SaveWorkspace(selected));
                }
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
            }
            "stop-confirmation" | "stop-confirmation-small" => {
                app.panel = Some(Panel::Stop(id(10)));
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
            }
            "workspace-accounts" | "workspace-accounts-small" => {
                app = account_fixture();
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
            }
            "workspaces-starting" => {
                let s = app.snapshot.as_mut().unwrap();
                s.vms[1].desired = Lifecycle::Running;
                let mut op = s.operations[0].clone();
                op.id = id(101);
                op.vm_id = id(11);
                op.phase = "applying".into();
                op.request = Mutation::Start {
                    vm_id: id(11),
                    without_gpu: false,
                };
                s.operations.push(op);
            }
            "workspaces-unavailable" => {
                app.snapshot_unavailable = true;
                app.error = Some(
                    "Host unavailable: connection interrupted. Showing the last snapshot.".into(),
                );
            }
            "workspace-stop-timeout" => {
                stop_timeout(&mut app);
                app.page = Page::Workspace(id(10));
            }
            "force-stop-confirmation" | "force-stop-confirmation-small" => {
                stop_timeout(&mut app);
                app.page = Page::Workspace(id(10));
                app.panel = Some(Panel::ForceStop(id(10)));
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
            }
            "workspaces-ready" => {
                app = codex_fixture("signed_in");
                app.page = Page::Workspaces;
                let s = app.snapshot.as_mut().unwrap();
                s.vms[0].gpus = vec!["0000:01:00.0".into()];
                s.vms[0].preferred_gpus = s.vms[0].gpus.clone();
                s.vms[1].owner = id(2);
                s.vms[1].preferred_gpus = vec!["0000:01:00.0".into()];
                s.me.committed.disk_bytes = 42 * GIB;
                s.me.grant.cpu_cores = 2;
                s.users[0].committed = s.me.committed;
                s.users[1].committed.disk_bytes = 42 * GIB;
            }
            "workspaces-attention" => {
                let s = app.snapshot.as_mut().unwrap();
                s.operations[0].phase = "needs_review".into();
                s.vms[0].error = Some(
                    "The guest did not finish applying its configuration. Inspect the operation before retrying.".into(),
                );
            }
            "workspaces-cleanup" => {
                let s = app.snapshot.as_mut().unwrap();
                let vm = &mut s.vms[1];
                vm.resources.memory_bytes = 4 * GIB;
                vm.resources.cpu_cores = 1;
                vm.cpu_ids = vec![5, 11];
                s.me.committed.memory_bytes = 8 * GIB;
                s.me.committed.cpu_cores = 2;
                s.users[0].committed = s.me.committed;
            }
            "workspaces-updating" => {
                app.snapshot.as_mut().unwrap().operations[0].phase = "applying".into();
            }
            "small-window" => size = (800.0, 600.0),
            "wide-window" => {
                size = (1440.0, 900.0);
                let vm = app.snapshot.as_ref().unwrap().vms[0].clone();
                let s = app.snapshot.as_mut().unwrap();
                for n in 20..24 {
                    let mut v = vm.clone();
                    v.id = id(n);
                    v.spec.name = if n == 21 {
                        "A deliberately long workspace name for layout checks".into()
                    } else {
                        format!("Project {}", n - 18)
                    };
                    s.vms.push(v);
                }
                s.me.grant = Resources {
                    memory_bytes: 32 * GIB,
                    cpu_cores: 8,
                    disk_bytes: 480 * GIB,
                };
                s.me.committed = Resources {
                    memory_bytes: 24 * GIB,
                    cpu_cores: 6,
                    disk_bytes: 252 * GIB,
                };
            }
            "member" => {
                let s = app.snapshot.as_mut().unwrap();
                s.me.role = Role::User;
                s.users = vec![s.me.clone()];
            }
            "empty" | "empty-small" => {
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                let s = app.snapshot.as_mut().unwrap();
                s.vms.clear();
                s.me.committed = Resources::default();
            }
            "over-quota" => app.snapshot.as_mut().unwrap().me.grant.memory_bytes = 2 * GIB,
            "host" => {}
            "host-small" => size = (800.0, 600.0),
            "host-selected" => app.host_owner = Some(id(1)),
            "host-allocations" | "host-storage" => {
                size = (1180.0, 1040.0);
                app.host_tab = HostTab::Allocations;
                if name == "host-storage" {
                    app.host_owner = Some(id(1));
                }
            }
            "host-over-quota" => {
                let s = app.snapshot.as_mut().unwrap();
                s.users[0].grant.memory_bytes = 4 * GIB;
                s.users[0].grant.cpu_cores = 1;
                s.me.grant = s.users[0].grant;
                app.host_owner = Some(id(1));
            }
            "host-member" => {
                app.snapshot.as_mut().unwrap().me.role = Role::User;
                // Deliberately leave the fixture's full users list in place:
                // the view must still scope the ledger and owner controls.
            }
            "host-gpu-releasing" => {
                app.snapshot.as_mut().unwrap().vms[0].desired = Lifecycle::Stopped
            }
            "host-hardware" => {
                app.host_tab = HostTab::Hardware;
                size = (1180.0, 1040.0);
            }
            "host-large-cpu" => {
                let s = app.snapshot.as_mut().unwrap();
                s.capacity.cpu_cores = 64;
                s.users[0].grant.cpu_cores = 32;
                s.users[1].grant.cpu_cores = 16;
                s.inventory.physical_cores = 72;
            }
            "host-zero-capacity" => {
                let s = app.snapshot.as_mut().unwrap();
                s.capacity = Resources::default();
                for u in &mut s.users {
                    u.grant = Resources::default();
                    u.committed = Resources::default();
                }
                s.vms.clear();
                s.me.grant = Resources::default();
                s.me.committed = Resources::default();
            }
            "settings" => app.page = Page::Settings,
            "hardware-setup" => {
                size = (1180.0, 1280.0);
                app.host_tab = HostTab::Hardware;
                app.hardware_details = true;
                let i = &mut app.snapshot.as_mut().unwrap().inventory;
                i.iommu = Some(Readiness {
                    state: "needs_firmware_configuration".into(),
                    evidence: vec!["DMAR present · no active IOMMU groups".into()],
                    next_steps: vec![
                        "Enable Intel VT-d in firmware, reboot, then refresh this page.".into(),
                        "If it is already enabled, inspect the kernel IOMMU configuration.".into(),
                    ],
                });
                i.gpus[0].readiness.state = "needs_firmware_configuration".into();
            }
            "people" => app.page = Page::People,
            "activity" => {
                app.page = Page::Activity;
                let s = app.snapshot.as_mut().unwrap();
                let mut op = s.operations[0].clone();
                op.id = id(101);
                op.request = Mutation::Gpu {
                    vm_id: id(10),
                    pci: Some("0000:01:00.0".into()),
                };
                op.phase = "needs_review".into();
                op.error=Some("Guest driver did not respond after restart. Inspect the guest before retrying.".into());
                s.operations.insert(0, op);
            }
            "create" => app.panel = Some(Panel::Create),
            "create-small" => {
                size = (800.0, 600.0);
                app.panel = Some(Panel::Create);
            }
            "resize" => {
                app.panel = Some(Panel::EditWorkspace(id(10)));
                app.fields[Field::Ram as usize] = "4".into();
                app.fields[Field::Disk as usize] = "40".into();
            }
            "workspace-settings"
            | "workspace-settings-small"
            | "workspace-settings-lower-small"
            | "workspace-member-settings" => {
                app.page = Page::Workspace(id(10));
                app.advanced = true;
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                if name == "workspace-settings-small" {
                    let vm = &mut app.snapshot.as_mut().unwrap().vms[0];
                    vm.gpus = vec!["0000:01:00.0".into()];
                    vm.preferred_gpus = vm.gpus.clone();
                }
                if name == "workspace-member-settings" {
                    let s = app.snapshot.as_mut().unwrap();
                    s.me.role = Role::User;
                    s.me.gpu_allowed = false;
                    s.users[0].gpu_allowed = false;
                }
            }
            "workspace-ready" | "workspace-ready-small" | "workspace-long-name" => {
                app = codex_fixture("signed_in");
                app.snapshot.as_mut().unwrap().applications[0]
                    .observation
                    .components
                    .push(ApplicationComponent {
                        name: "Claude Code".into(),
                        version: Some("2.1.308".into()),
                        ready: true,
                        account: "signed_in".into(),
                    });
                if name.ends_with("small") || name == "workspace-long-name" {
                    size = (800.0, 600.0);
                }
                if name == "workspace-long-name" {
                    app.snapshot.as_mut().unwrap().vms[0].spec.name =
                        "A deliberately long workspace name for layout checks".into();
                }
            }
            "workspace-gpu-preference" => {
                app.page = Page::Workspace(id(11));
                app.advanced = true;
                app.snapshot.as_mut().unwrap().vms[1].preferred_gpus = vec!["0000:01:00.0".into()];
            }
            "workspace-stopped" => {
                app.snapshot.as_mut().unwrap().vms[1].preferred_gpus = vec!["0000:01:00.0".into()];
                app.page = Page::Workspace(id(11));
            }
            "workspace-cleanup" => {
                let vm = &mut app.snapshot.as_mut().unwrap().vms[1];
                vm.resources.memory_bytes = 4 * GIB;
                vm.resources.cpu_cores = 1;
                vm.cpu_ids = vec![5, 11];
                app.page = Page::Workspace(id(11));
            }
            "gpu-unavailable" | "gpu-unavailable-small" => {
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                app.page = Page::Workspace(id(11));
                app.snapshot.as_mut().unwrap().vms[1].preferred_gpus = vec!["0000:01:00.0".into()];
                app.panel = Some(Panel::GpuUnavailable(GpuUnavailable {
                    vm_id: id(11),
                    pci: "0000:01:00.0".into(),
                    reason: "It is currently claimed by another VM.".into(),
                }));
            }
            "stopping" => {
                let s = app.snapshot.as_mut().unwrap();
                s.vms[0].desired = Lifecycle::Stopped;
                s.vms[0].gpus = vec!["0000:01:00.0".into()];
                s.vms[0].preferred_gpus = s.vms[0].gpus.clone();
                s.operations[0].phase = "applying".into();
                s.operations[0].request = Mutation::State {
                    vm_id: id(10),
                    desired: Lifecycle::Stopped,
                };
            }
            "t3-setup" => app.page = Page::Workspace(id(10)),
            "t3-codex-submitting"
            | "t3-codex-queued"
            | "t3-codex-verifying"
            | "t3-claude-waiting"
            | "t3-claude-waiting-small"
            | "t3-claude-verifying"
            | "t3-details" => {
                app = codex_fixture(if name.starts_with("t3-claude") || name == "t3-details" {
                    "signed_in"
                } else {
                    "signed_out"
                });
                app.snapshot.as_mut().unwrap().applications[0]
                    .observation
                    .components
                    .push(ApplicationComponent {
                        name: "Claude Code".into(),
                        version: Some("2.1.308".into()),
                        ready: true,
                        account: "signed_out".into(),
                    });
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                if name == "t3-details" {
                    app.advanced = true;
                    app.app_details = true;
                    size = (1180.0, 1040.0);
                } else if name == "t3-codex-submitting" {
                    app.app_request = Some(ApplicationRequest {
                        id: id(200),
                        target: Some(ApplicationSetupAction::SignInCodex),
                        submitting_code: false,
                    });
                } else {
                    let target = if name.starts_with("t3-claude") {
                        ApplicationSetupAction::SignInClaude
                    } else {
                        ApplicationSetupAction::SignInCodex
                    };
                    let verifying = name.ends_with("verifying");
                    let phase = if verifying {
                        ApplicationPhase::Running
                    } else if name.ends_with("queued") {
                        ApplicationPhase::Pending
                    } else {
                        ApplicationPhase::WaitingForUser
                    };
                    app.snapshot
                        .as_mut()
                        .unwrap()
                        .application_operations
                        .push(setup_operation(target, phase));
                    let observation =
                        &mut app.snapshot.as_mut().unwrap().applications[0].observation;
                    observation.operation_id = Some(id(201));
                    observation.operation_stage = if verifying {
                        "verifying"
                    } else {
                        "awaiting_approval"
                    }
                    .into();
                    if target == ApplicationSetupAction::SignInClaude && !verifying {
                        observation.action = Some(BrowserAction {
                            url: "https://claude.com/cai/oauth/authorize?fixture=true".into(),
                            code: String::new(),
                            expires_at: u64::MAX,
                            setup_action: target,
                            input_required: true,
                        });
                        app.browser_opened = Some(id(200));
                    }
                }
            }
            "t3-codex-waiting"
            | "t3-codex-waiting-small"
            | "t3-codex-signed-in"
            | "t3-codex-interrupted" => {
                app = codex_fixture(if name == "t3-codex-signed-in" {
                    "signed_in"
                } else {
                    "signed_out"
                });
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                if name.starts_with("t3-codex-waiting") {
                    app.snapshot.as_mut().unwrap().applications[0]
                        .observation
                        .operation_id = Some(id(201));
                    app.snapshot.as_mut().unwrap().applications[0]
                        .observation
                        .action = Some(BrowserAction {
                        url: "https://auth.openai.com/codex/device".into(),
                        code: "DEMO-1234".into(),
                        expires_at: u64::MAX,
                        setup_action: ApplicationSetupAction::SignInCodex,
                        input_required: false,
                    });
                    app.snapshot.as_mut().unwrap().application_operations.push(
                        ApplicationOperation {
                            id: id(201),
                            application_id: id(200),
                            actor: id(1),
                            kind: ApplicationOperationKind::Setup,
                            setup_action: ApplicationSetupAction::SignInCodex,
                            phase: ApplicationPhase::WaitingForUser,
                        },
                    );
                } else if name == "t3-codex-interrupted" {
                    app.snapshot
                        .as_mut()
                        .unwrap()
                        .application_operations
                        .push(setup_operation(
                            ApplicationSetupAction::SignInCodex,
                            ApplicationPhase::Interrupted,
                        ));
                    app.snapshot.as_mut().unwrap().applications[0]
                        .observation
                        .operation_id = Some(id(201));
                    app.snapshot.as_mut().unwrap().applications[0]
                        .observation
                        .operation_message =
                        "Codex sign-in was interrupted. Request a new sign-in link.".into();
                }
            }
            "t3-providers-missing" | "t3-providers-installed" => {
                app.page = Page::Workspace(id(10));
                let installed = name.ends_with("installed");
                app.snapshot
                    .as_mut()
                    .unwrap()
                    .applications
                    .push(ManagedApplication {
                        id: id(200),
                        vm_id: id(10),
                        kind: ApplicationKind::T3,
                        desired_version: "0.1.2".into(),
                        start_on_boot: true,
                        observation: ApplicationObservation {
                            installed_version: Some("0.1.2".into()),
                            service: "running".into(),
                            setup: "ready".into(),
                            message: "Connected to T3.".into(),
                            components: vec![
                                ApplicationComponent {
                                    name: "Codex CLI".into(),
                                    version: installed.then(|| "0.165.0".into()),
                                    ready: installed,
                                    account: if installed {
                                        "signed_out"
                                    } else {
                                        "unavailable"
                                    }
                                    .into(),
                                },
                                ApplicationComponent {
                                    name: "Claude Code".into(),
                                    version: installed.then(|| "2.1.308".into()),
                                    ready: installed,
                                    account: if installed {
                                        "signed_out"
                                    } else {
                                        "unavailable"
                                    }
                                    .into(),
                                },
                            ],
                            ..Default::default()
                        },
                    });
            }
            "t3-waiting" | "t3-interrupted" | "t3-connected" | "t3-stopped"
            | "t3-waiting-small" => {
                if name.ends_with("small") {
                    size = (800.0, 600.0);
                }
                app.page = Page::Workspace(id(10));
                let waiting = name.starts_with("t3-waiting");
                let stopped = name == "t3-stopped";
                let s = app.snapshot.as_mut().unwrap();
                if stopped {
                    s.vms[0].desired = Lifecycle::Stopped;
                    s.vms[0].observed = "stopped".into();
                    s.vms[0].resources.memory_bytes = 0;
                    s.vms[0].resources.cpu_cores = 0;
                    s.vms[0].cpu_ids.clear();
                }
                s.applications.push(ManagedApplication {
                    id: id(200),
                    vm_id: id(10),
                    kind: ApplicationKind::T3,
                    desired_version: "0.1.2".into(),
                    start_on_boot: true,
                    observation: ApplicationObservation {
                        installed_version: Some("0.1.2".into()),
                        service: "running".into(),
                        setup: if waiting {
                            "waiting_for_user"
                        } else if name == "t3-connected" {
                            "ready"
                        } else {
                            "interrupted"
                        }
                        .into(),
                        message: if waiting {
                            "Approve this VM in your browser"
                        } else if name == "t3-connected" {
                            "Account connected; remote connectivity is not currently confirmed."
                        } else {
                            "Setup interrupted. Request a new authorization link."
                        }
                        .into(),
                        operation_id: waiting.then(|| id(201)),
                        action: waiting.then(|| BrowserAction {
                            url: "https://example.com/fixture-only".into(),
                            code: "DEMO-1234".into(),
                            expires_at: u64::MAX,
                            setup_action: ApplicationSetupAction::ConnectT3,
                            input_required: false,
                        }),
                        ..Default::default()
                    },
                });
                if waiting {
                    s.application_operations.push(ApplicationOperation {
                        id: id(201),
                        application_id: id(200),
                        actor: id(1),
                        kind: ApplicationOperationKind::Setup,
                        setup_action: ApplicationSetupAction::ConnectT3,
                        phase: ApplicationPhase::WaitingForUser,
                    });
                }
            }
            "delete" => app.panel = Some(Panel::Delete(id(10))),
            "pairing" => app.snapshot = None,
            _ => {}
        }
        let mut rendered = simulator(&app, size);
        if name == "host-storage-disks-small" {
            scroll_bottom(&mut rendered, size);
        }
        if matches!(
            name,
            "host-reserves-pending" | "host-reserves-review" | "host-reserves-unapplied"
        ) {
            scroll_bottom(&mut rendered, size);
        }
        if name == "host-memory-policy-status" {
            scroll_bottom(&mut rendered, size);
        }
        if name == "host-memory-policy-cache-small" {
            rendered.point_at((400.0, 300.0));
            rendered.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
                delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -4000.0 },
            })]);
        }
        if name.starts_with("host-headroom") {
            scroll_bottom(&mut rendered, size);
        }
        if name == "workspace-edit-gpu-small" {
            rendered.point_at((400.0, 300.0));
            rendered.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
                delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -4000.0 },
            })]);
        }
        if matches!(
            name,
            "t3-claude-waiting-small"
                | "t3-codex-waiting-small"
                | "t3-waiting-small"
                | "workspace-settings-lower-small"
                | "workspaces-fab-bottom-small"
                | "workspace-fab-bottom-small"
        ) {
            rendered.point_at((size.0 - 100.0, size.1 - 100.0));
            rendered.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
                delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -2000.0 },
            })]);
        }
        if name == "workspace-name-hover" {
            let _ = rendered.snapshot(&app.theme()).unwrap();
            let target = rendered
                .find(iced::widget::Id::from(format!("vm-name-{}", id(10))))
                .unwrap();
            rendered.point_at(target.bounds().center());
            rendered.simulate([iced::Event::Window(iced::window::Event::RedrawRequested(
                iced::time::Instant::now() + std::time::Duration::from_millis(200),
            ))]);
        }
        if name == "lifecycle-tooltip" {
            // Prime the theme's reduced-motion timings before opening the hint.
            let _ = rendered.snapshot(&app.theme()).unwrap();
            let target = rendered.find(lifecycle_id(id(11))).unwrap();
            rendered.point_at(target.bounds().center());
            let now = iced::time::Instant::now();
            rendered.simulate([iced::Event::Window(iced::window::Event::RedrawRequested(
                now,
            ))]);
            rendered.simulate([iced::Event::Window(iced::window::Event::RedrawRequested(
                now + std::time::Duration::from_secs(1),
            ))]);
        }
        let snapshot = rendered.snapshot(&app.theme()).unwrap();
        let filename = format!("{name}-tiny-skia.png");
        let actual = artifacts.join(&filename);
        let expected = baseline.join(&filename);
        if actual.exists() {
            fs::remove_file(&actual).unwrap();
        }
        assert!(
            snapshot
                .matches_image(artifacts.join(format!("{name}.png")))
                .unwrap()
        );
        if update && expected.exists() {
            fs::remove_file(&expected).unwrap();
        }
        if !expected.exists() && !update {
            failures.push(format!(
                "{name}: missing baseline (run UPDATE_SCREENSHOTS=1)"
            ));
            continue;
        }
        if !snapshot
            .matches_image(baseline.join(format!("{name}.png")))
            .unwrap()
        {
            let a = image::open(&actual).unwrap().into_rgba8();
            let e = image::open(&expected).unwrap().into_rgba8();
            let mut diff = a.clone();
            for (x, y, pixel) in diff.enumerate_pixels_mut() {
                let changed =
                    e.dimensions() != a.dimensions() || e.get_pixel(x, y) != a.get_pixel(x, y);
                *pixel = if changed {
                    image::Rgba([255, 70, 100, 255])
                } else {
                    image::Rgba([20, 24, 30, 255])
                };
            }
            diff.save(artifacts.join(format!("{name}-diff.png")))
                .unwrap();
            failures.push(name.into());
        }
    }
    assert!(
        failures.is_empty(),
        "Screenshot differences: {}. Inspect target/ui-screenshots for actual and diff images; update baselines only after visual review.",
        failures.join(", ")
    );
}

#[test]
fn navigation_and_dialogs_publish_the_right_actions() {
    let _guard = lock();
    let mut app = fixture();
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("New VM").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::Open(Panel::Create)]));
    let _ = app.update(messages[0].clone());
    assert!(matches!(app.panel, Some(Panel::Create)));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Cancel").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Close]
    ));
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(11))));
    assert_eq!(app.get(Field::Ram), "4");
    assert_eq!(app.get(Field::Disk), "40");
    let mut member = fixture();
    member.snapshot.as_mut().unwrap().me.role = Role::User;
    let mut ui = simulator(&member, (800.0, 600.0));
    assert!(ui.find("People").is_err());
}

#[test]
fn management_actions_keep_the_selected_vm_and_deletion_is_separate() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::OpenWorkspace(id(11)));
    let _ = app.update(Msg::ToggleAdvanced);
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Edit VM").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::Open(Panel::EditWorkspace(vm))] if *vm == id(11)));
    let _ = app.update(messages[0].clone());
    let _ = app.update(Msg::Edit(Field::Name, "Renamed experiments".into()));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Save changes").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::SaveWorkspace(vm)] if *vm == id(11))
    );
    let _ = app.update(Msg::OpenWorkspace(id(11)));
    let _ = app.update(Msg::ToggleAdvanced);
    let mut ui = simulator(&app, (1180.0, 820.0));
    scroll_bottom(&mut ui, (1180.0, 820.0));
    ui.click(iced::widget::Id::from(format!("vm-delete-{}", id(11))))
        .unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    let _ = app.update(messages[0].clone());
    assert!(matches!(app.panel, Some(Panel::Delete(vm)) if vm == id(11)));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Cancel").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Close]
    ));
}

#[test]
fn small_window_keeps_key_controls_and_counts_unclipped() {
    let _guard = lock();
    let mut app = fixture();
    let mut ui = simulator(&app, (800.0, 600.0));
    for label in ["New VM", "2 total", "Open terminal"] {
        let target = ui.find(label).unwrap();
        let bounds = target.bounds();
        let visible = target.visible_bounds().unwrap();
        assert!(
            visible.width >= bounds.width - 1.0 && visible.height >= bounds.height - 1.0,
            "{label} is clipped: {bounds:?} vs {visible:?}"
        );
    }
    drop(ui);
    let _ = app.update(Msg::Open(Panel::Create));
    let mut ui = simulator(&app, (800.0, 600.0));
    for label in ["Cancel", "Create VM"] {
        let target = ui.find(label).unwrap();
        assert!(target.visible_bounds().unwrap().height >= target.bounds().height - 1.0);
    }
}

#[test]
fn workspace_cards_offer_the_next_safe_action_for_the_selected_machine() {
    let _guard = lock();
    let mut app = fixture();
    app.snapshot.as_mut().unwrap().me.grant.cpu_cores = 2;
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("1 core available").is_ok());
    ui.click("Open terminal").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Ssh(vm, false)] if *vm == id(10)
    ));
    app.snapshot.as_mut().unwrap().vms[1].preferred_gpus = vec!["0000:01:00.0".into()];
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click(lifecycle_id(id(11))).unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Change(Mutation::Start { vm_id, without_gpu: false })] if *vm_id == id(11)
    ));
    let vm = &mut app.snapshot.as_mut().unwrap().vms[1];
    vm.resources.memory_bytes = 4 * GIB;
    vm.resources.cpu_cores = 1;
    vm.cpu_ids = vec![5, 11];
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Start VM").is_err());
    ui.click(lifecycle_id(id(11))).unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Change(Mutation::State { vm_id, desired: Lifecycle::Stopped })] if *vm_id == id(11)
    ));
}

#[test]
fn workspace_cards_block_duplicate_actions_and_route_attention_to_details() {
    let _guard = lock();
    let mut app = fixture();
    app.snapshot.as_mut().unwrap().operations[0].phase = "applying".into();
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Open terminal").is_err());
    assert!(ui.find("Running").is_err());
    ui.click(lifecycle_id(id(10))).unwrap();
    assert!(ui.into_messages().next().is_none());
    let s = app.snapshot.as_mut().unwrap();
    s.vms[0].desired = Lifecycle::Stopped;
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click(lifecycle_id(id(10))).unwrap();
    assert!(ui.into_messages().next().is_none());
    app.snapshot.as_mut().unwrap().operations[0].phase = "needs_review".into();
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Needs attention").is_ok());
    ui.click("Review issue").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::OpenWorkspace(vm)] if *vm == id(10)
    ));
    app.busy = true;
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click(lifecycle_id(id(11))).unwrap();
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn workspace_cards_distinguish_t3_readiness_from_installation_and_vm_state() {
    let _guard = lock();
    let mut app = codex_fixture("signed_in");
    app.page = Page::Workspaces;
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Ubuntu · ready in T3").is_ok());
    drop(ui);
    app.snapshot.as_mut().unwrap().applications[0]
        .observation
        .components[0]
        .account = "signed_out".into();
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Ubuntu · ready in T3").is_err());
    assert!(ui.find("Ubuntu · T3 setup needed").is_ok());
    drop(ui);
    let s = app.snapshot.as_mut().unwrap();
    s.applications[0].observation.components[0].account = "signed_in".into();
    s.vms[0].desired = Lifecycle::Stopped;
    s.vms[0].observed = "stopped".into();
    s.vms[0].resources.memory_bytes = 0;
    s.vms[0].resources.cpu_cores = 0;
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Ubuntu · ready in T3").is_err());
    assert!(ui.find("Ubuntu · T3 installed").is_ok());
}

#[test]
fn gpu_conflict_requires_explicit_consent_and_back_keeps_the_vm_stopped() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::OpenWorkspace(id(11)));
    app.snapshot.as_mut().unwrap().vms[1].preferred_gpus = vec!["0000:01:00.0".into()];
    let conflict = GpuUnavailable {
        vm_id: id(11),
        pci: "0000:01:00.0".into(),
        reason: "Claimed by another allocation".into(),
    };
    let _ = app.update(Msg::Reply(Ok(Response::GpuUnavailable(conflict.clone()))));
    assert!(matches!(&app.panel, Some(Panel::GpuUnavailable(c)) if c.vm_id == id(11)));
    assert!(!app.snapshot.as_ref().unwrap().vms[1].has_compute());
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Go back").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::Close]));
    let _ = app.update(messages[0].clone());
    assert_eq!(app.page, Page::Workspace(id(11)));
    assert!(app.panel.is_none());
    assert_eq!(
        app.snapshot.as_ref().unwrap().vms[1].desired,
        Lifecycle::Stopped
    );
    let _ = app.update(Msg::Reply(Ok(Response::GpuUnavailable(conflict))));
    let mut ui = simulator(&app, (800.0, 600.0));
    let button = ui.find("Start without GPUs").unwrap();
    assert!(button.bounds().y + button.bounds().height <= 600.0);
    ui.click("Start without GPUs").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::StartWithoutGpu(vm)] if *vm == id(11))
    );
    assert!(
        !app.snapshot.as_ref().unwrap().vms[1]
            .preferred_gpus
            .is_empty()
    );
}

#[test]
fn legacy_stopped_claims_have_an_explicit_cleanup_action() {
    let _guard = lock();
    let mut app = fixture();
    let vm = &mut app.snapshot.as_mut().unwrap().vms[1];
    vm.resources.memory_bytes = 4 * GIB;
    vm.resources.cpu_cores = 1;
    vm.cpu_ids = vec![5, 11];
    let _ = app.update(Msg::OpenWorkspace(id(11)));
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click(lifecycle_id(id(11))).unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::Change(Mutation::State { vm_id, desired: Lifecycle::Stopped })] if *vm_id == id(11))
    );
}

#[test]
fn hosts_navigation_owner_highlight_and_ledger_preserve_selection() {
    let _guard = lock();
    let mut app = host_fixture();
    let mut ui = simulator(&app, (1180.0, 820.0));
    for label in [
        "24 GiB",
        "4 cores",
        "8 TiB",
        "4 GiB unissued",
        "5 TiB unissued",
    ] {
        let target = ui.find(label).unwrap();
        let bounds = target.bounds();
        let visible = target.visible_bounds().unwrap();
        assert!(
            visible.width >= bounds.width - 1.0 && visible.height >= bounds.height - 1.0,
            "{label} is clipped"
        );
    }
    ui.click("Tommy").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::HighlightOwner(Some(owner))] if *owner == id(1)));
    let _ = app.update(messages[0].clone());
    assert_eq!(app.host_owner, Some(id(1)));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Allocations").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(
        messages.as_slice(),
        [Msg::HostTab(HostTab::Allocations)]
    ));
    let _ = app.update(messages[0].clone());
    let mut ui = simulator(&app, (1180.0, 820.0));
    for label in ["Memory", "CPU", "Storage"] {
        assert!(ui.find(label).is_ok());
    }
    drop(ui);
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click(iced::widget::Id::from(format!("host-grant-{}", id(2))))
        .unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::Select(owner)] if *owner == id(2)));
    let _ = app.update(messages[0].clone());
    assert_eq!(app.selected, Some(id(2)));
    assert!(matches!(app.panel, Some(Panel::Grant)));
}

#[test]
fn member_host_views_hide_other_users_and_setup_details_stay_in_hardware() {
    let _guard = lock();
    let mut app = host_fixture();
    app.snapshot.as_mut().unwrap().me.role = Role::User;
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Alex Chen").is_err());
    assert!(ui.find("Show passthrough diagnostics").is_err());
    assert!(ui.find("Reduce motion").is_err());
    drop(ui);
    let _ = app.update(Msg::HostTab(HostTab::Allocations));
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Alex Chen").is_err());
    assert!(ui.find("Alex's workspace").is_err());
    assert!(ui.find("Edit grant").is_err());
    drop(ui);
    let _ = app.update(Msg::HostTab(HostTab::Hardware));
    let mut ui = simulator(&app, (1180.0, 1000.0));
    ui.click("Show passthrough diagnostics").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::ToggleHardware]
    ));
    let _ = app.update(Msg::Navigate(Page::Settings));
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Reduce motion").is_ok());
}

#[test]
fn application_controls_target_the_selected_app_and_hide_expired_authorization() {
    let _guard = lock();
    let mut app = fixture();
    app.page = Page::Workspace(id(10));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Install T3 Code").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::InstallApplication(vm)] if *vm==id(10))
    );
    app.snapshot
        .as_mut()
        .unwrap()
        .applications
        .push(ManagedApplication {
            id: id(200),
            vm_id: id(10),
            kind: ApplicationKind::T3,
            desired_version: "0.1.2".into(),
            start_on_boot: true,
            observation: ApplicationObservation {
                installed_version: Some("0.1.2".into()),
                setup: "interrupted".into(),
                ..Default::default()
            },
        });
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Get a new authorization link").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::SetupApplication(a, ApplicationSetupAction::ConnectT3)] if *a==id(200))
    );
    app.snapshot.as_mut().unwrap().applications[0]
        .observation
        .action = Some(BrowserAction {
        url: "https://example.com".into(),
        code: "EXPIRED".into(),
        expires_at: 0,
        setup_action: ApplicationSetupAction::ConnectT3,
        input_required: false,
    });
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Continue in browser").is_err());
}

#[test]
fn missing_provider_tools_have_an_explicit_install_action_after_pairing() {
    let _guard = lock();
    let mut app = fixture();
    app.page = Page::Workspace(id(10));
    app.snapshot
        .as_mut()
        .unwrap()
        .applications
        .push(ManagedApplication {
            id: id(200),
            vm_id: id(10),
            kind: ApplicationKind::T3,
            desired_version: "0.1.2".into(),
            start_on_boot: true,
            observation: ApplicationObservation {
                installed_version: Some("0.1.2".into()),
                setup: "ready".into(),
                components: vec![ApplicationComponent {
                    name: "Codex CLI".into(),
                    version: None,
                    ready: false,
                    account: "unavailable".into(),
                }],
                ..Default::default()
            },
        });
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Install provider tools").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::InstallApplication(vm)] if *vm==id(10))
    );
}

fn codex_fixture(account: &str) -> App {
    let mut app = fixture();
    app.page = Page::Workspace(id(10));
    app.snapshot
        .as_mut()
        .unwrap()
        .applications
        .push(ManagedApplication {
            id: id(200),
            vm_id: id(10),
            kind: ApplicationKind::T3,
            desired_version: "0.1.2".into(),
            start_on_boot: true,
            observation: ApplicationObservation {
                installed_version: Some("0.1.2".into()),
                service: "running".into(),
                setup: "ready".into(),
                message: "Connected to T3.".into(),
                components: vec![ApplicationComponent {
                    name: "Codex CLI".into(),
                    version: Some("0.165.0".into()),
                    ready: true,
                    account: account.into(),
                }],
                ..Default::default()
            },
        });
    app
}

#[test]
fn codex_signin_targets_a_named_owner_action_and_does_not_offer_account_replacement() {
    let _guard = lock();
    let app = codex_fixture("signed_out");
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Sign into Codex").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::SetupApplication(a,ApplicationSetupAction::SignInCodex)] if *a==id(200))
    );
    for account in ["signed_in", "unknown", "unavailable"] {
        let app = codex_fixture(account);
        let mut ui = simulator(&app, (1180.0, 820.0));
        assert!(ui.find("Sign into Codex").is_err());
    }
    let mut app = codex_fixture("signed_out");
    app.snapshot.as_mut().unwrap().me.id = id(2);
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Sign into Codex").is_err());
}

fn setup_operation(
    target: ApplicationSetupAction,
    phase: ApplicationPhase,
) -> ApplicationOperation {
    ApplicationOperation {
        id: id(201),
        application_id: id(200),
        actor: id(1),
        kind: ApplicationOperationKind::Setup,
        setup_action: target,
        phase,
    }
}

#[test]
fn sign_in_feedback_starts_immediately_survives_ack_and_a_stale_snapshot() {
    let _guard = lock();
    let mut app = codex_fixture("signed_out");
    let stale = app.snapshot.clone().unwrap();
    let _ = app.update(Msg::SetupApplication(
        id(200),
        ApplicationSetupAction::SignInCodex,
    ));
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Preparing Codex sign-in…").is_ok());
    drop(ui);
    let op = setup_operation(
        ApplicationSetupAction::SignInCodex,
        ApplicationPhase::Pending,
    );
    let _ = app.update(Msg::Reply(Ok(Response::ApplicationOperation(op.clone()))));
    assert!(!app.busy);
    let _ = app.update(Msg::Snapshot(Ok(Box::new(stale))));
    assert!(
        app.snapshot
            .as_ref()
            .unwrap()
            .application_operations
            .iter()
            .any(|x| x.id == op.id)
    );
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Preparing Codex sign-in…").is_ok());
}

#[test]
fn returned_code_is_cleared_on_submit_and_verification_is_visible_before_polling() {
    let _guard = lock();
    let mut app = codex_fixture("signed_in");
    app.authorization_input = "FIXTURE-RETURN-CODE".into();
    let _ = app.update(Msg::SubmitAuthorization(id(200), id(201)));
    assert!(app.authorization_input.is_empty());
    let mut ui = simulator(&app, (1180.0, 1000.0));
    assert!(ui.find("Sending authorization code…").is_ok());
    drop(ui);
    let _ = app.update(Msg::Reply(Ok(Response::Ok)));
    app.snapshot
        .as_mut()
        .unwrap()
        .application_operations
        .push(setup_operation(
            ApplicationSetupAction::SignInClaude,
            ApplicationPhase::WaitingForUser,
        ));
    let mut ui = simulator(&app, (1180.0, 1000.0));
    assert!(ui.find("Checking Claude sign-in…").is_ok());
}

#[test]
fn workspace_overview_keeps_accounts_compact_and_technical_details_in_settings() {
    let _guard = lock();
    let mut app = codex_fixture("signed_out");
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Apps & accounts").is_ok());
    assert!(ui.find("T3 Code").is_ok());
    assert!(ui.find("T3 connection").is_err());
    assert!(ui.find("Codex").is_ok());
    assert!(ui.find("Connect T3 account").is_err());
    assert!(ui.find("Codex CLI · 0.165.0").is_err());
    assert!(ui.find("Manage VM").is_err());
    assert!(ui.find("Show application details").is_err());
    assert!(ui.find("Linked to T3").is_err());
    drop(ui);
    app.snapshot.as_mut().unwrap().applications[0]
        .observation
        .message = "Account connected; remote connectivity is not currently confirmed.".into();
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(
        ui.find("Link saved; remote connectivity is not currently confirmed.")
            .is_ok()
    );
    drop(ui);
    let _ = app.update(Msg::ToggleAdvanced);
    let mut ui = simulator(&app, (1180.0, 1000.0));
    reveal(&mut ui, "Show application details");
    ui.click("Show application details").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::ToggleAppDetails]
    ));
    let _ = app.update(Msg::ToggleAppDetails);
    let mut ui = simulator(&app, (1180.0, 1000.0));
    assert!(ui.find("Codex CLI · 0.165.0").is_ok());
}

fn expand_advanced(ui: &mut Simulator<'_, Msg, Theme>) {
    ui.point_at((500.0, 500.0));
    ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
        delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -4000.0 },
    })]);
    ui.click("Show advanced").unwrap();
}

#[test]
fn workspace_details_has_one_destination_and_header_actions_keep_the_selected_vm() {
    let _guard = lock();
    let mut app = fixture();
    for (label, selected) in [("T3 development", id(10)), ("Experiments", id(11))] {
        let mut ui = simulator(&app, (1180.0, 820.0));
        assert!(ui.find("Details").is_err());
        ui.click(label).unwrap();
        assert!(matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),
            [Msg::OpenWorkspace(vm)] if *vm == selected));
    }
    let _ = app.update(Msg::OpenWorkspace(id(10)));
    assert_eq!(app.page, Page::Workspace(id(10)));
    assert!(app.panel.is_none());
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Manage").is_err());
    assert!(ui.find("T3 settings").is_err());
    ui.click(lifecycle_id(id(10))).unwrap();
    assert!(matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Open(Panel::Stop(vm_id))] if *vm_id == id(10)));
    let _ = app.update(Msg::OpenWorkspace(id(11)));
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click(lifecycle_id(id(11))).unwrap();
    assert!(matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Change(Mutation::Start { vm_id, without_gpu: false })] if *vm_id == id(11)));
    for (vm_id, label, cleanup) in [
        (id(10), "Stop VM", false),
        (id(11), "Start VM", false),
        (id(11), "Finish stopping", true),
    ] {
        let _ = app.update(Msg::OpenWorkspace(vm_id));
        if cleanup {
            let vm = &mut app.snapshot.as_mut().unwrap().vms[1];
            vm.resources.memory_bytes = 4 * GIB;
            vm.resources.cpu_cores = 1;
        }
        for size in [(800.0, 600.0), (1180.0, 820.0)] {
            let mut ui = simulator(&app, size);
            let target = ui.find(lifecycle_id(vm_id)).unwrap();
            let bounds = target.bounds();
            let visible = target.visible_bounds().unwrap();
            assert!(
                visible.width >= bounds.width - 1.0
                    && visible.height >= bounds.height - 1.0
                    && bounds.x + bounds.width <= size.0
                    && bounds.y + bounds.height <= size.1,
                "{label} clipped at {size:?}: {bounds:?} vs {visible:?}"
            );
        }
    }
}

#[test]
fn advanced_details_preserve_pending_sign_in_and_unsubmitted_return_codes() {
    let _guard = lock();
    let mut app = codex_fixture("signed_in");
    let s = app.snapshot.as_mut().unwrap();
    s.application_operations.push(setup_operation(
        ApplicationSetupAction::SignInClaude,
        ApplicationPhase::WaitingForUser,
    ));
    s.applications[0].observation.operation_id = Some(id(201));
    s.applications[0].observation.action = Some(BrowserAction {
        url: "https://claude.com/cai/oauth/authorize?fixture=true".into(),
        code: String::new(),
        expires_at: u64::MAX,
        setup_action: ApplicationSetupAction::SignInClaude,
        input_required: true,
    });
    app.authorization_input = "FIXTURE-UNSUBMITTED-CODE".into();
    let mut ui = simulator(&app, (1180.0, 820.0));
    expand_advanced(&mut ui);
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::ToggleAdvanced]));
    let _ = app.update(messages[0].clone());
    assert_eq!(app.authorization_input, "FIXTURE-UNSUBMITTED-CODE");
    assert_eq!(app.page, Page::Workspace(id(10)));
    assert!(app.panel.is_none());
    assert_eq!(
        app.snapshot.as_ref().unwrap().application_operations[0].phase,
        ApplicationPhase::WaitingForUser
    );
    let mut ui = simulator(&app, (1180.0, 820.0));
    scroll_bottom(&mut ui, (1180.0, 820.0));
    reveal(&mut ui, "Hide advanced");
    ui.click("Hide advanced").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    let _ = app.update(messages[0].clone());
    let mut ui = simulator(&app, (1180.0, 1040.0));
    assert!(ui.find("Continue in browser").is_ok());
    assert!(
        app.snapshot.as_ref().unwrap().applications[0]
            .observation
            .action
            .is_some()
    );
}

#[test]
fn successful_provider_setup_announces_completion_and_collapses_the_browser_flow() {
    let _guard = lock();
    let mut app = codex_fixture("signed_out");
    app.snapshot
        .as_mut()
        .unwrap()
        .application_operations
        .push(setup_operation(
            ApplicationSetupAction::SignInCodex,
            ApplicationPhase::WaitingForUser,
        ));
    let mut next = app.snapshot.clone().unwrap();
    next.application_operations[0].phase = ApplicationPhase::Succeeded;
    next.applications[0].observation.components[0].account = "signed_in".into();
    let _ = app.update(Msg::Snapshot(Ok(Box::new(next))));
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(
        ui.find("Codex sign-in complete. You can use it in T3 Code.")
            .is_ok()
    );
    assert!(ui.find("Signed in · account email unavailable").is_ok());
    assert!(ui.find("Continue in browser").is_err());
    assert!(ui.find("Preparing Codex sign-in…").is_err());
}

#[test]
fn small_workspace_keeps_header_accounts_and_scrolled_settings_actions_visible() {
    let _guard = lock();
    let mut app = codex_fixture("signed_in");
    app.snapshot.as_mut().unwrap().applications[0]
        .observation
        .components
        .push(ApplicationComponent {
            name: "Claude Code".into(),
            version: Some("2.1.308".into()),
            ready: true,
            account: "signed_in".into(),
        });
    let mut ui = simulator(&app, (800.0, 600.0));
    reveal(&mut ui, "Claude");
    for label in ["Open terminal", "Claude"] {
        let target = ui.find(label).unwrap();
        let bounds = target.bounds();
        let visible = target.visible_bounds().unwrap();
        assert!(
            visible.width >= bounds.width - 1.0 && visible.height >= bounds.height - 1.0,
            "{label} clipped: {bounds:?} vs {visible:?}"
        );
    }
    expand_advanced(&mut ui);
    let messages = ui.into_messages().collect::<Vec<_>>();
    let _ = app.update(messages[0].clone());
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Edit VM").unwrap();
    assert!(matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Open(Panel::EditWorkspace(vm))] if *vm == id(10)));
    let mut ui = simulator(&app, (800.0, 600.0));
    scroll_bottom(&mut ui, (800.0, 600.0));
    let control = ui.find(lifecycle_id(id(10))).unwrap();
    assert!(control.visible_bounds().unwrap().height >= control.bounds().height - 1.0);
    let target = ui
        .find(iced::widget::Id::from(format!("vm-delete-{}", id(10))))
        .unwrap();
    assert!(target.visible_bounds().unwrap().height >= target.bounds().height - 1.0);
}

#[test]
fn claude_terminal_fixture_urls_pass_the_real_desktop_browser_guard() {
    let fixtures: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../guest/fixtures/claude-auth-terminal.json"
    ))
    .unwrap();
    for fixture in fixtures {
        if let Some(url) = fixture["url"].as_str() {
            assert!(
                validate_authorization_url(url).is_ok(),
                "{}",
                fixture["name"]
            );
        }
    }
    for url in [
        "http://claude.com/cai/oauth/authorize",
        "https://claude.com/path\u{7}https://claude.com/path",
        "https://claude.com/path\nnext",
        "\u{1b}]8;;https://claude.com/path\u{7}",
    ] {
        assert!(validate_authorization_url(url).is_err());
    }
}

fn stop_timeout(app: &mut App) {
    let s = app.snapshot.as_mut().unwrap();
    s.vms[0].desired = Lifecycle::Stopped;
    s.vms[0].error =
        Some("Graceful shutdown did not complete. Resource allocations remain held.".into());
    s.operations[0].request = Mutation::State {
        vm_id: id(10),
        desired: Lifecycle::Stopped,
    };
    s.operations[0].phase = "needs_review".into();
}

#[test]
fn force_stop_is_separate_confirmed_and_keep_waiting_remains_graceful() {
    let _guard = lock();
    let mut app = fixture();
    stop_timeout(&mut app);
    let _ = app.update(Msg::OpenWorkspace(id(10)));
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Shutdown hasn’t completed").is_ok());
    ui.click("Keep waiting").unwrap();
    assert!(matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Change(Mutation::State { vm_id, desired: Lifecycle::Stopped })] if *vm_id == id(10)));
    let _ = app.update(Msg::ForceStop(id(10)));
    assert!(app.vm_request.is_none());
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Force stop…").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::Open(Panel::ForceStop(vm))] if *vm == id(10)));
    let _ = app.update(messages[0].clone());
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Cancel").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(app.vm_request.is_none());
    let _ = app.update(Msg::Open(Panel::ForceStop(id(10))));
    let _ = app.update(Msg::ForceStop(id(11)));
    assert!(app.vm_request.is_none());
    let mut ui = simulator(&app, (800.0, 600.0));
    let confirm = ui.find("Force stop").unwrap();
    assert!(confirm.visible_bounds().unwrap().height >= confirm.bounds().height - 1.0);
    ui.click("Force stop").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(
        matches!(&app.vm_request, Some((vm, Mutation::ForceStop { vm_id })) if *vm == id(10) && *vm_id == id(10))
    );
}

#[test]
fn lifecycle_progress_starts_immediately_survives_stale_snapshots_and_offline_is_unknown() {
    let _guard = lock();
    let mut app = fixture();
    let stale = app.snapshot.clone().unwrap();
    let request = Mutation::State {
        vm_id: id(10),
        desired: Lifecycle::Stopped,
    };
    let _ = app.update(Msg::Change(request.clone()));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click(lifecycle_id(id(10))).unwrap();
    assert!(ui.into_messages().next().is_none());
    let op = Operation {
        progress: None,
        id: id(600),
        vm_id: id(10),
        actor: id(1),
        request,
        phase: "pending".into(),
        error: None,
    };
    let _ = app.update(Msg::Reply(Ok(Response::Operation(op))));
    let _ = app.update(Msg::Snapshot(Ok(Box::new(stale.clone()))));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click(lifecycle_id(id(10))).unwrap();
    assert!(ui.into_messages().next().is_none());
    let _ = app.update(Msg::Snapshot(Err("offline".into())));
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Status unavailable").is_ok());
    ui.click(lifecycle_id(id(11))).unwrap();
    assert!(ui.into_messages().next().is_none());
    let _ = app.update(Msg::Snapshot(Ok(Box::new(stale))));
    assert!(!app.snapshot_unavailable);
}

#[test]
fn lifecycle_icon_has_a_keyboard_action_and_focus_tooltip() {
    let _guard = lock();
    let app = fixture();
    let mut ui = simulator(&app, (1180.0, 820.0));
    let bounds = ui.find(lifecycle_id(id(11))).unwrap().bounds();
    let mut reached = false;
    for _ in 0..24 {
        ui.tap_key(iced::keyboard::key::Named::Tab);
        reached = ui
            .find(move |candidate: iced_test::selector::Candidate<'_>| {
                if let iced_test::selector::Candidate::Focusable {
                    bounds: candidate_bounds,
                    state,
                    ..
                } = candidate
                    && candidate_bounds.contains(bounds.center())
                {
                    return Some(state.is_focused());
                }
                None
            })
            .unwrap_or(false);
        if reached {
            break;
        }
    }
    assert!(reached, "The lifecycle icon must be reachable by Tab");
    ui.point_at((0.0, 0.0));
    let now = iced::time::Instant::now();
    ui.simulate([iced::Event::Window(iced::window::Event::RedrawRequested(
        now,
    ))]);
    ui.simulate([iced::Event::Window(iced::window::Event::RedrawRequested(
        now + std::time::Duration::from_secs(1),
    ))]);
    assert!(ui.find("Start VM · Experiments").is_ok());
    ui.tap_key(iced::keyboard::key::Named::Enter);
    assert!(matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Change(Mutation::Start { vm_id, without_gpu: false })] if *vm_id == id(11)));
}

fn account_fixture() -> App {
    let mut app = codex_fixture("signed_in");
    let observation = &mut app.snapshot.as_mut().unwrap().applications[0].observation;
    observation.components.push(ApplicationComponent {
        name: "Claude Code".into(),
        version: Some("2.1.308".into()),
        ready: true,
        account: "signed_in".into(),
    });
    observation.accounts = vec![
        ApplicationAccount {
            name: "T3".into(),
            account_id: Some("user_fixture_t3".into()),
            ..Default::default()
        },
        ApplicationAccount {
            name: "Codex CLI".into(),
            email: Some("developer@example.com".into()),
            plan: Some("pro".into()),
            authentication: Some("chatgpt".into()),
            ..Default::default()
        },
        ApplicationAccount {
            name: "Claude Code".into(),
            email: Some("developer@example.org".into()),
            plan: Some("max".into()),
            ..Default::default()
        },
    ];
    app
}

#[test]
fn graceful_stop_requires_confirmation_and_cancel_does_not_request_shutdown() {
    let _guard = lock();
    let mut app = fixture();
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click(lifecycle_id(id(10))).unwrap();
    let msg = ui.into_messages().next().unwrap();
    assert!(matches!(msg, Msg::Open(Panel::Stop(vm)) if vm == id(10)));
    let _ = app.update(msg);
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Cancel").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(app.vm_request.is_none());
    let _ = app.update(Msg::Stop(id(10)));
    assert!(app.vm_request.is_none());
    let _ = app.update(Msg::Open(Panel::Stop(id(10))));
    let _ = app.update(Msg::Stop(id(11)));
    assert!(app.vm_request.is_none());
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Stop VM").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(
        matches!(app.vm_request, Some((vm, Mutation::State { desired: Lifecycle::Stopped, .. })) if vm == id(10))
    );
}

#[test]
fn workspace_name_target_includes_the_header_space_and_stays_separate_from_stop() {
    let _guard = lock();
    let app = fixture();
    let mut ui = simulator(&app, (1180.0, 820.0));
    let name = ui.find("T3 development").unwrap().bounds();
    let region = ui
        .find(iced::widget::Id::from(format!("vm-name-{}", id(10))))
        .unwrap()
        .bounds();
    let stop = ui.find(lifecycle_id(id(10))).unwrap().bounds();
    assert!(region.height >= 48.0 && region.width > name.width + 24.0);
    assert!(region.x + region.width <= stop.x);
    ui.point_at(iced::Point::new(
        region.x + region.width - 8.0,
        region.center_y(),
    ));
    ui.simulate([
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)),
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
            iced::mouse::Button::Left,
        )),
    ]);
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::OpenWorkspace(vm)] if *vm == id(10))
    );
}

#[test]
fn unified_workspace_has_one_resource_editor_and_owner_account_metadata() {
    let _guard = lock();
    let app = account_fixture();
    let mut ui = simulator(&app, (1180.0, 1000.0));
    assert!(ui.find("Overview").is_err());
    assert!(ui.find("Ready to use").is_err());
    assert!(ui.find("Available in T3 Code on your Mac.").is_err());
    assert!(ui.find("developer@example.com · Pro").is_ok());
    assert!(ui.find("developer@example.org · Max").is_ok());
    assert!(ui.find("Linked to T3").is_err());
    assert!(
        ui.find("Account linked · email not provided by T3")
            .is_err()
    );
    assert!(ui.find("Account-wide allowance").is_err());
    assert!(
        ui.find("Account allowance is not available from the CLI.")
            .is_err()
    );
    assert!(ui.find("View usage in browser").is_err());
    ui.click("Edit VM").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::Open(Panel::EditWorkspace(vm))] if *vm == id(10))
    );
    let mut app = account_fixture();
    app.snapshot.as_mut().unwrap().me.id = id(2);
    let mut ui = simulator(&app, (1180.0, 1000.0));
    assert!(ui.find("developer@example.com · Pro").is_err());
    assert!(ui.find("72% remaining").is_err());
}

fn reveal(ui: &mut Simulator<'_, Msg, Theme>, label: &str) {
    let viewport = ui
        .find(iced::widget::Id::new("page-body"))
        .unwrap()
        .bounds();
    // Finder bounds are in content coordinates. Scan from the top instead of
    // treating them as translated screen coordinates after a scroll.
    ui.point_at(viewport.center());
    ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
        delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: 10000.0 },
    })]);
    for _ in 0..20 {
        let target = ui.find(label).unwrap();
        if target
            .visible_bounds()
            .is_some_and(|visible| visible.height >= target.bounds().height - 1.0)
        {
            return;
        }
        ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
            delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -240.0 },
        })]);
    }
    panic!("{label} could not be revealed by scrolling");
}

#[test]
fn workspace_edit_shows_restart_impact_and_confirms_a_frozen_vm_specific_request() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("No restart needed").is_err());
    ui.click("Save changes").unwrap();
    assert!(ui.into_messages().next().is_none());
    let _ = app.update(Msg::Edit(Field::Name, "My renamed box".into()));
    let _ = app.update(Msg::Edit(Field::Ram, "8".into()));
    let _ = app.update(Msg::EditGpu(GpuEdit::Set { pci: None }));
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Restart required").is_err());
    let action = ui.find("Save & restart…").unwrap();
    assert!(action.visible_bounds().unwrap().height >= action.bounds().height - 1.0);
    ui.click("Save & restart…").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(app.vm_request.is_none());
    assert!(matches!(app.panel,Some(Panel::ConfirmEdit{vm_id,..}) if vm_id==id(10)));
    let _ = app.update(Msg::ConfirmWorkspaceEdit(id(11)));
    assert!(app.vm_request.is_none());
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Go back").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert_eq!(app.get(Field::Name), "My renamed box");
    assert_eq!(app.get(Field::Ram), "8");
    assert!(matches!(app.edit_gpu, GpuEdit::Set { pci: None }));
    assert!(app.vm_request.is_none());
    let _ = app.update(Msg::SaveWorkspace(id(10)));
    // The confirmation uses the reviewed draft, not any later field mutation.
    let _ = app.update(Msg::Edit(Field::Ram, "16".into()));
    let mut ui = simulator(&app, (800.0, 600.0));
    let action = ui.find("Save & restart").unwrap();
    assert!(action.visible_bounds().unwrap().height >= action.bounds().height - 1.0);
    ui.click("Save & restart").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(
        matches!(&app.vm_request,Some((vm,Mutation::Edit{vm_id,changes,restart_confirmed:true})) if *vm==id(10)&&*vm_id==id(10)&&changes.memory_gib==8)
    );
    let _ = app.update(Msg::Reply(Err("Not enough resource capacity".into())));
    let mut ui = simulator(&app, (800.0, 600.0));
    let error = ui.find("Not enough resource capacity").unwrap();
    assert!(error.visible_bounds().unwrap().height >= error.bounds().height - 1.0);
}
#[test]
fn workspace_name_only_and_stopped_edits_do_not_request_a_restart_and_cancel_has_no_effect() {
    let _guard = lock();
    for selected in [id(10), id(11)] {
        let mut app = fixture();
        let _ = app.update(Msg::Open(Panel::EditWorkspace(selected)));
        let _ = app.update(Msg::Edit(Field::Name, "New name".into()));
        let mut ui = simulator(&app, (800.0, 600.0));
        assert!(ui.find("Save changes").is_ok());
        ui.click("Save changes").unwrap();
        let _ = app.update(ui.into_messages().next().unwrap());
        assert!(
            matches!(&app.vm_request,Some((vm,Mutation::Edit{vm_id,restart_confirmed:false,..})) if *vm==selected&&*vm_id==selected)
        );
    }
    let mut app = fixture();
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(11))));
    let _ = app.update(Msg::Edit(Field::Ram, "8".into()));
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Save changes").is_ok());
    ui.click("Cancel").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    let _ = app.update(Msg::SaveWorkspace(id(11)));
    assert!(app.vm_request.is_none());
}

#[test]
fn gpu_selection_is_reachable_in_the_small_edit_dialog_and_combines_with_memory_changes() {
    let _guard = lock();
    let mut app = fixture();
    let vm = &mut app.snapshot.as_mut().unwrap().vms[0];
    vm.gpus = vec!["0000:01:00.0".into()];
    vm.preferred_gpus = vm.gpus.clone();
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
    let _ = app.update(Msg::Edit(Field::Ram, "2".into()));
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(
        ui.find("GPUs require at least 4 GiB RAM. Increase memory or clear the GPU selection.")
            .is_err()
    );
    ui.click("Save & restart…").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(
        matches!(&app.panel,Some(Panel::ConfirmEdit{changes,..}) if changes.memory_gib==2 && matches!(changes.gpu,GpuEdit::Keep))
    );
    assert!(app.vm_request.is_none());
    let _ = app.update(Msg::BackToEdit(id(10)));
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.point_at((400.0, 300.0));
    ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
        delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -4000.0 },
    })]);
    let option = ui.find("GeForce GTX 1070 Ti · 0000:01:00.0").unwrap();
    assert!(option.visible_bounds().unwrap().height >= option.bounds().height - 1.0);
    ui.click("GeForce GTX 1070 Ti · 0000:01:00.0").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(matches!(&app.edit_gpu, GpuEdit::SetMany { pcis } if pcis.is_empty()));
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Restart required").is_err());
    ui.click("Save & restart…").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(
        ui.find("All GPUs will be detached and remembered preferences cleared.")
            .is_ok()
    );
    ui.click("Save & restart").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(
        matches!(&app.vm_request,Some((_,Mutation::Edit{changes,..})) if changes.memory_gib==2&&matches!(&changes.gpu,GpuEdit::SetMany{pcis} if pcis.is_empty()))
    );
}

fn add_second_gpu(app: &mut App) {
    let s = app.snapshot.as_mut().unwrap();
    let mut second = s.inventory.gpus[0].clone();
    second.pci = "0000:02:00.0".into();
    second.model = "NVIDIA Corporation [GeForce RTX 3060]".into();
    second.assigned_vm = None;
    second.readiness.state = "ready".into();
    s.inventory.gpus.push(second);
}
#[test]
fn edit_geometry_stays_fixed_through_rename_resize_validation_and_gpu_selection() {
    let _guard = lock();
    let mut app = fixture();
    add_second_gpu(&mut app);
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
    let geometry = |app: &App| {
        let mut ui = simulator(app, (1180.0, 820.0));
        ["Edit VM", "Resources", "GPUs"].map(|label| ui.find(label).unwrap().bounds().y)
    };
    let original = geometry(&app);
    let save_y = simulator(&app, (1180.0, 820.0))
        .find("Save changes")
        .unwrap()
        .bounds()
        .y;
    for event in [
        Msg::Edit(Field::Name, "Renamed".into()),
        Msg::Edit(Field::Ram, "8".into()),
        Msg::ToggleEditGpu("0000:01:00.0".into(), true),
        Msg::ToggleEditGpu("0000:02:00.0".into(), true),
        Msg::Edit(Field::Ram, "0".into()),
        Msg::ToggleEditGpu("0000:01:00.0".into(), false),
        Msg::ToggleEditGpu("0000:02:00.0".into(), false),
    ] {
        let _ = app.update(event);
        assert_eq!(geometry(&app), original);
        let mut ui = simulator(&app, (1180.0, 820.0));
        let label = if ui.find("Save & restart…").is_ok() {
            "Save & restart…"
        } else {
            "Save changes"
        };
        let save = ui.find(label).unwrap();
        assert_eq!(save.bounds().y, save_y);
        assert!(ui.find("Keep current setting").is_err());
        assert!(ui.find("No restart needed").is_err());
        assert!(ui.find("Restart required").is_err());
    }
}
#[test]
fn gpu_checkboxes_select_multiple_and_undo_preserves_the_original_preference() {
    let _guard = lock();
    let mut app = fixture();
    add_second_gpu(&mut app);
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
    for label in [
        "GeForce GTX 1070 Ti · 0000:01:00.0",
        "GeForce RTX 3060 · 0000:02:00.0",
    ] {
        let mut ui = simulator(&app, (1180.0, 820.0));
        ui.click(label).unwrap();
        let _ = app.update(ui.into_messages().next().unwrap());
    }
    assert!(matches!(&app.edit_gpu,GpuEdit::SetMany{pcis} if pcis.len()==2));
    assert!(simulator(&app, (1180.0, 820.0)).find("2 selected").is_ok());
    let _ = app.update(Msg::SaveWorkspace(id(10)));
    assert!(
        matches!(&app.panel,Some(Panel::ConfirmEdit{changes,..}) if matches!(&changes.gpu,GpuEdit::SetMany{pcis} if pcis.len()==2))
    );
    let _ = app.update(Msg::BackToEdit(id(10)));
    for address in ["0000:01:00.0", "0000:02:00.0"] {
        let _ = app.update(Msg::ToggleEditGpu(address.into(), false));
    }
    assert!(matches!(app.edit_gpu, GpuEdit::Keep));
}

#[test]
fn resource_sliders_support_mouse_keyboard_and_precise_numeric_entry_without_applying() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
    let mut ui = simulator(&app, (1180.0, 1000.0));
    let slider = ui
        .find(iced::widget::Id::new("resource-slider-memory"))
        .unwrap()
        .bounds();
    ui.point_at(slider.center());
    ui.simulate(iced_test::simulator::click());
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Msg::ResourceValue(Field::Ram, 9)))
    );
    for message in messages {
        let _ = app.update(message);
    }
    assert_eq!(app.get(Field::Ram), "9");
    let mut ui = simulator(&app, (1180.0, 1000.0));
    let slider = ui
        .find(iced::widget::Id::new("resource-slider-memory"))
        .unwrap()
        .bounds();
    ui.point_at(iced::Point::new(
        slider.x + 24.0 + (slider.width - 48.0) * 8.0 / 15.0,
        slider.center_y(),
    ));
    ui.simulate(iced_test::simulator::click());
    ui.simulate(iced_test::simulator::tap_key(
        iced::keyboard::key::Named::ArrowRight,
        None,
    ));
    for message in ui.into_messages() {
        let _ = app.update(message);
    }
    assert_eq!(app.get(Field::Ram), "10");
    let mut ui = simulator(&app, (1180.0, 1000.0));
    ui.click(iced::widget::Id::new("resource-value-memory"))
        .unwrap();
    ui.simulate(iced_test::simulator::tap_key(
        iced::keyboard::key::Named::Home,
        None,
    ));
    for _ in 0..2 {
        ui.simulate(iced_test::simulator::tap_key(
            iced::keyboard::key::Named::Delete,
            None,
        ));
    }
    ui.typewrite("7");
    for message in ui.into_messages() {
        let _ = app.update(message);
    }
    assert_eq!(app.get(Field::Ram), "7");
    assert!(app.vm_request.is_none());
    let _ = app.update(Msg::SaveWorkspace(id(10)));
    assert!(matches!(&app.panel,Some(Panel::ConfirmEdit{changes,..}) if changes.memory_gib==7));
}
#[test]
fn disk_slider_stops_at_existing_size_and_typed_shrink_cannot_be_saved() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
    let _ = app.update(Msg::Edit(Field::Disk, "80".into()));
    let mut ui = simulator(&app, (1180.0, 1000.0));
    let slider = ui
        .find(iced::widget::Id::new("resource-slider-disk"))
        .unwrap()
        .bounds();
    ui.point_at((slider.x + 1.0, slider.center_y()));
    ui.simulate(iced_test::simulator::click());
    for message in ui.into_messages() {
        let _ = app.update(message);
    }
    assert_eq!(app.get(Field::Disk), "40");
    let _ = app.update(Msg::Edit(Field::Disk, "39".into()));
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Save changes").unwrap();
    assert!(ui.into_messages().next().is_none());
    assert!(app.vm_request.is_none());
}
#[test]
fn provider_identity_shares_the_name_row_and_long_accounts_wrap_without_clipping() {
    let _guard = lock();
    let app = account_fixture();
    for size in [(1180.0, 1000.0), (800.0, 900.0)] {
        let mut ui = simulator(&app, size);
        for (provider, account) in [
            ("Codex", "developer@example.com · Pro"),
            ("Claude", "developer@example.org · Max"),
        ] {
            let label = ui.find(provider).unwrap().bounds();
            let identity = ui.find(account).unwrap().bounds();
            assert!((label.center_y() - identity.center_y()).abs() < 3.0);
            assert!(identity.x > label.x + label.width);
        }
    }
    let mut app = account_fixture();
    let long = "a.deliberately.long.development.account.address.for.layout.coverage@example-development.test";
    app.snapshot.as_mut().unwrap().applications[0]
        .observation
        .accounts[1]
        .email = Some(long.into());
    let mut ui = simulator(&app, (800.0, 1000.0));
    let text = ui.find(format!("{long} · Pro")).unwrap();
    let bounds = text.bounds();
    assert!(bounds.x + bounds.width <= 800.0);
    assert!(bounds.height > 20.0);
}

pub(super) fn setup_fixture(review: bool) -> App {
    let mut app = fixture();
    app.page = Page::Workspace(id(10));
    let s = app.snapshot.as_mut().unwrap();
    s.operations.clear();
    let vm = &mut s.vms[0];
    vm.observed = "pending".into();
    s.operations.push(Operation {
        progress: Some(VmSetupProgress {
            stage: VmSetupStage::GuestSetup,
            since: 123,
        }),
        id: id(700),
        vm_id: vm.id,
        actor: s.me.id,
        request: Mutation::Create {
            spec: vm.spec.clone(),
        },
        phase: if review { "needs_review" } else { "retrying" }.into(),
        error: Some(
            if review {
                "conflict: Guest package installation failed. Check networking and setup logs."
            } else {
                "pending: Waiting for guest provisioning."
            }
            .into(),
        ),
    });
    app
}

#[test]
fn setup_recovery_keeps_retry_explicit_and_incomplete_delete_confirmed() {
    let _guard = lock();
    let mut app = setup_fixture(true);
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("VM setup needs attention").is_ok());
    assert!(ui.find("Installing guest tools").is_ok());
    ui.click("Retry after repair").unwrap();
    assert!(matches!(ui.into_messages().next(), Some(Msg::Retry(_))));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Delete incomplete VM…").unwrap();
    let _ = app.update(ui.into_messages().next().unwrap());
    assert!(matches!(app.panel, Some(Panel::Delete(_))));
    assert!(app.vm_request.is_none());
    let mut member = setup_fixture(true);
    member.snapshot.as_mut().unwrap().me.role = Role::User;
    let mut ui = simulator(&member, (1180.0, 820.0));
    assert!(ui.find("Retry after repair").is_err());
}

#[test]
fn late_setup_logs_do_not_reopen_a_dialog_or_replace_another_vms_logs() {
    let mut app = fixture();
    let report = || VmDiagnostics {
        vm_id: id(10),
        progress: None,
        power_state: "running".into(),
        checks: vec![],
        logs: vec![],
    };
    app.panel = Some(Panel::SetupDiagnostics(id(10)));
    let _ = app.update(Msg::Close);
    let _ = app.update(Msg::SetupDiagnosticsLoaded(id(10), Ok(report())));
    assert!(app.panel.is_none());
    app.panel = Some(Panel::SetupDiagnostics(id(11)));
    app.setup_diagnostics = None;
    let _ = app.update(Msg::SetupDiagnosticsLoaded(id(10), Ok(report())));
    assert!(app.setup_diagnostics.is_none());
}

#[test]
fn pending_setup_messages_do_not_move_the_details_button_between_retries() {
    let _guard = lock();
    let mut app = setup_fixture(false);
    let before = simulator(&app, (800.0, 600.0))
        .find("View setup details")
        .unwrap()
        .bounds();
    app.snapshot.as_mut().unwrap().operations[0].error = None;
    app.snapshot.as_mut().unwrap().operations[0].phase = "applying".into();
    let after = simulator(&app, (800.0, 600.0))
        .find("View setup details")
        .unwrap()
        .bounds();
    assert_eq!(before, after);
}
