use super::*;
use crate::visual_tests::{fixture, host_fixture, id, lock, simulator};

#[test]
fn activity_distinguishes_terminal_queued_active_and_unknown_phases() {
    let _guard = lock();
    for (phase, label) in [
        ("succeeded", "Completed"),
        ("failed", "Failed"),
        ("needs_review", "Needs review"),
        ("pending", "Queued"),
        ("applying", "In progress"),
        ("retrying", "In progress"),
        ("future_phase", "Status unknown"),
    ] {
        let mut app = fixture();
        app.page = Page::Activity;
        let op = &mut app.snapshot.as_mut().unwrap().operations[0];
        op.phase = phase.into();
        if phase == "failed" {
            op.request = Mutation::State {
                vm_id: op.vm_id,
                desired: Lifecycle::Stopped,
            };
            op.error = Some("Superseded by an explicit shutdown/deletion request.".into());
        }
        let mut ui = simulator(&app, (1180.0, 820.0));
        assert!(ui.find(label).is_ok(), "Missing {label} for {phase}");
        if label != "In progress" {
            assert!(ui.find("In progress").is_err(), "{phase} appears active");
        }
        if phase == "failed" {
            assert!(
                ui.find("Superseded by an explicit shutdown/deletion request.")
                    .is_ok()
            );
            assert!(ui.find("Retry after review").is_err());
        }
    }
}

#[test]
fn allocations_and_vm_headers_agree_on_recovery_and_inflight_states() {
    let _guard = lock();
    for (scenario, label) in [
        ("cleanup", "Cleanup needed"),
        ("offline", "Status unavailable"),
        ("blocked", "Startup blocked"),
        ("request", "Updating…"),
        ("active", "Updating…"),
        ("review", "Needs attention"),
    ] {
        let mut app = host_fixture();
        app.host_tab = HostTab::Allocations;
        app.host_owner = Some(id(2));
        let s = app.snapshot.as_mut().unwrap();
        let vm = s.vms.iter_mut().find(|vm| vm.id == id(12)).unwrap();
        match scenario {
            "cleanup" => {
                vm.desired = Lifecycle::Stopped;
                vm.observed = "stopped".into();
            }
            "offline" => app.snapshot_unavailable = true,
            "blocked" => {
                vm.observed = "stopped".into();
                s.memory_health = Some(HostMemoryHealth {
                    condition: MemoryCondition::AllocationsExceedCapacity,
                    details: None,
                });
            }
            "request" => {
                app.vm_request = Some((
                    vm.id,
                    Mutation::Gpu {
                        vm_id: vm.id,
                        pci: None,
                    },
                ));
            }
            "active" | "review" => {
                s.operations.push(Operation {
                    progress: None,
                    id: id(101),
                    vm_id: vm.id,
                    actor: id(2),
                    request: Mutation::Gpu {
                        vm_id: vm.id,
                        pci: None,
                    },
                    phase: if scenario == "active" {
                        "applying"
                    } else {
                        "needs_review"
                    }
                    .into(),
                    error: None,
                });
            }
            _ => unreachable!(),
        }
        let mut ui = simulator(&app, (1180.0, 1040.0));
        assert!(
            ui.find(format!(
                "2 GiB memory · 1 core CPU · 80 GiB storage · {label}"
            ))
            .is_ok(),
            "Allocations omitted {label} for {scenario}"
        );
        drop(ui);
        app.page = Page::Workspace(id(12));
        assert!(simulator(&app, (1180.0, 1040.0)).find(label).is_ok());
    }
}

#[test]
fn settings_has_no_refresh_action_even_during_background_refresh() {
    let _guard = lock();
    let mut app = fixture();
    app.page = Page::Settings;
    for refreshing in [false, true] {
        app.refreshing = refreshing;
        let mut ui = simulator(&app, (800.0, 600.0));
        assert!(ui.find("Reduce motion").is_ok());
        assert!(ui.find("Refresh").is_err());
        assert!(ui.find("Refreshing…").is_err());
    }
    app.page = Page::Activity;
    app.refreshing = false;
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Refresh").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Refresh]
    ));
}
