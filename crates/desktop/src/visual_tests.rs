use super::*;
use iced::{Settings, Size};
use iced_test::Simulator;
use std::{
    fs,
    path::Path,
    sync::{Mutex, OnceLock},
};

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn fixture() -> App {
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
            memory_bytes: 8 * GIB,
            cpu_cores: 2,
            disk_bytes: 84 * GIB,
        },
        gpu_allowed: true,
    };
    let vm = |n, name: &str, state: &str| Vm {
        id: id(n),
        owner: me.id,
        spec: VmSpec {
            name: name.into(),
            memory_gib: 4,
            cores: 1,
            disk_gib: 40,
            ssh_public_key: "ssh-ed25519 FIXTURE_PUBLIC_KEY".into(),
        },
        resources: Resources {
            memory_bytes: 4 * GIB,
            cpu_cores: 1,
            disk_bytes: 42 * GIB,
        },
        cpu_ids: vec![4, 10],
        network_slot: n as u16,
        desired: if state == "running" {
            Lifecycle::Running
        } else {
            Lifecycle::Stopped
        },
        observed: state.into(),
        ssh_host_key: Some("ssh-ed25519 FIXTURE_HOST_KEY".into()),
        gpu: None,
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
        hostname: "homecloud-01".into(),
        cpu_model: "Intel Core i7-8700K".into(),
        physical_cores: 6,
        memory_bytes: 32 * GIB,
        pool: "tank".into(),
        pool_available_bytes: 620 * GIB,
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
fn simulator(app: &App, size: (f32, f32)) -> Simulator<'_, Msg, Theme> {
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
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
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
        "workspaces",
        "small-window",
        "wide-window",
        "member",
        "empty",
        "over-quota",
        "host",
        "hardware-setup",
        "people",
        "activity",
        "create",
        "create-small",
        "resize",
        "manage",
        "t3-setup",
        "delete",
        "pairing",
    ];
    let mut failures = vec![];
    for name in names {
        let mut app = fixture();
        let mut size = (1180.0, 820.0);
        match name {
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
            "empty" => {
                let s = app.snapshot.as_mut().unwrap();
                s.vms.clear();
                s.me.committed = Resources::default();
            }
            "over-quota" => app.snapshot.as_mut().unwrap().me.grant.memory_bytes = 4 * GIB,
            "host" => app.page = Page::Host,
            "hardware-setup" => {
                app.page = Page::Host;
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
                app.panel = Some(Panel::Resize(id(10)));
                app.fields[Field::Ram as usize] = "4".into();
                app.fields[Field::Disk as usize] = "40".into();
            }
            "manage" => app.panel = Some(Panel::Vm(id(10))),
            "t3-setup" => app.panel = Some(Panel::T3(id(10))),
            "delete" => app.panel = Some(Panel::Delete(id(10))),
            "pairing" => app.snapshot = None,
            _ => {}
        }
        let snapshot = simulator(&app, size).snapshot(&app.theme()).unwrap();
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
    ui.click("+  New VM").unwrap();
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
    let _ = app.update(Msg::Open(Panel::Resize(id(11))));
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
    let _ = app.update(Msg::Open(Panel::Vm(id(11))));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Resize").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::Open(Panel::Resize(vm))] if *vm == id(11)));
    let _ = app.update(messages[0].clone());
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Apply & restart").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(), [Msg::Resize(vm)] if *vm == id(11))
    );
    let _ = app.update(Msg::Open(Panel::Vm(id(11))));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Delete VM").unwrap();
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
    for label in ["+  New VM", "2 total", "Open terminal"] {
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
