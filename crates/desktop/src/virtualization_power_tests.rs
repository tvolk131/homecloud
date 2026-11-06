use super::*;
use crate::{
    virtualization::{Cockpit, Confirmation, Effect, Event, GuestIdentity, PowerRequest},
    virtualization_ui_tests::fixture,
    visual_tests::{id, lock, simulator},
};

pub(super) fn guest() -> VirtualizationGuestIdentity {
    VirtualizationGuestIdentity {
        node: "demo-proxmox".into(),
        vmid: 101,
        kind: VirtualizationGuestKind::VirtualMachine,
    }
}

pub(super) fn operation(phase: VirtualizationOperationPhase) -> VirtualizationOperation {
    VirtualizationOperation {
        id: id(810),
        requested_by: "48f3b6c988ec4df7a497da7f5b11fcb880d5aafce0e1c5ff523302a27c9bb412".into(),
        guest: guest(),
        action: VirtualizationPowerAction::Shutdown,
        phase,
        task_id: matches!(
            phase,
            VirtualizationOperationPhase::Running
                | VirtualizationOperationPhase::Succeeded
                | VirtualizationOperationPhase::Failed
        )
        .then(|| {
            "UPID:demo-proxmox:00000001:00000002:00000003:qmshutdown:101:owner@pve!homecloud:"
                .into()
        }),
        error: None,
        created_at: 1_800_000_000,
        updated_at: 1_800_000_005,
    }
}

fn receive(
    cockpit: &mut Cockpit,
    response: Result<Vec<VirtualizationOperation>, String>,
) -> Effect {
    let (session, generation, revision) = cockpit.begin_operations_refresh();
    cockpit.update(Event::Operations {
        session,
        generation,
        revision,
        response,
    })
}

fn submit(cockpit: &mut Cockpit, action: VirtualizationPowerAction) -> PowerRequest {
    assert_eq!(
        cockpit.update(Event::ReviewPower(guest(), action)),
        Effect::None
    );
    let token = cockpit.confirmation.as_ref().unwrap().token();
    assert!(cockpit.pending.is_none());
    let Effect::Submit(request) = cockpit.update(Event::Confirm(token)) else {
        panic!("Expected an explicitly confirmed submit");
    };
    assert!(cockpit.pending.as_ref().unwrap().in_flight);
    assert_eq!(cockpit.update(Event::Confirm(token)), Effect::None);
    request
}

fn inspect(app: &mut App, vmid: u32) {
    let _ = app.update(Msg::Cockpit(Event::Inspect(GuestIdentity {
        node: "demo-proxmox".into(),
        vmid,
    })));
}

#[test]
fn running_controls_open_confirmation_and_cancel_without_submitting() {
    let _guard = lock();
    let mut app = fixture();
    inspect(&mut app, 101);
    let mut ui = simulator(&app, (800.0, 1200.0));
    assert!(ui.find("Start").is_err());
    ui.click("Shut down").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(messages.iter().any(|message| matches!(
        message,
        Msg::Cockpit(Event::ReviewPower(_, VirtualizationPowerAction::Shutdown))
    )));
    for message in messages {
        let _ = app.update(message);
    }
    assert!(app.cockpit.as_ref().unwrap().pending.is_none());
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Shut down VM 101 on demo-proxmox?").is_ok());
    let confirm = ui.find("Confirm shut down").unwrap().bounds();
    assert!(confirm.y + confirm.height <= 600.0);
    ui.click("Cancel").unwrap();
    for message in ui.into_messages() {
        let _ = app.update(message);
    }
    assert!(app.cockpit.as_ref().unwrap().confirmation.is_none());
    assert!(app.cockpit.as_ref().unwrap().pending.is_none());
}

#[test]
fn stopped_qemu_has_start_and_no_shutdown_or_force_controls() {
    let _guard = lock();
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let guest = &mut cockpit.inventory.as_mut().unwrap().guests[0];
    guest.status = "stopped".into();
    guest.memory_used_bytes = Some(8 * GIB);
    inspect(&mut app, 101);
    let mut ui = simulator(&app, (800.0, 1200.0));
    assert!(ui.find("Shut down").is_err());
    assert!(ui.find("Force stop…").is_err());
    ui.click("Start").unwrap();
    for message in ui.into_messages() {
        let _ = app.update(message);
    }
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Start VM 101 on demo-proxmox?").is_ok());
    drop(ui);
    let cockpit = app.cockpit.as_mut().unwrap();
    let token = cockpit.confirmation.as_ref().unwrap().token();
    assert!(
        matches!(cockpit.update(Event::Confirm(token)), Effect::Submit(request) if request.action == VirtualizationPowerAction::Start)
    );
}

#[test]
fn force_stop_has_a_distinct_danger_warning_and_second_confirmation() {
    let _guard = lock();
    let mut app = fixture();
    inspect(&mut app, 101);
    let mut ui = simulator(&app, (800.0, 1200.0));
    ui.click("Force stop…").unwrap();
    for message in ui.into_messages() {
        let _ = app.update(message);
    }
    assert!(app.cockpit.as_ref().unwrap().pending.is_none());
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Force stop VM 101 on demo-proxmox?").is_ok());
    assert!(ui.find("Abruptly cuts power. Unsaved data may be lost and guest filesystems may be damaged. Prefer graceful Shut down.").is_ok());
    let confirm = ui.find("Confirm force stop").unwrap().bounds();
    assert!(confirm.y + confirm.height <= 600.0);
    ui.click("Confirm force stop").unwrap();
    let events = ui.into_messages().collect::<Vec<_>>();
    let cockpit = app.cockpit.as_mut().unwrap();
    let event = events
        .into_iter()
        .find_map(|message| {
            if let Msg::Cockpit(event) = message {
                Some(event)
            } else {
                None
            }
        })
        .unwrap();
    assert!(
        matches!(cockpit.update(event), Effect::Submit(request) if request.action == VirtualizationPowerAction::ForceStop)
    );
}

#[test]
fn template_and_container_have_no_power_buttons_and_forged_events_are_rejected() {
    let _guard = lock();
    for (vmid, kind) in [
        (9000, VirtualizationGuestKind::VirtualMachine),
        (201, VirtualizationGuestKind::Container),
    ] {
        let mut app = fixture();
        inspect(&mut app, vmid);
        let mut ui = simulator(&app, (800.0, 1200.0));
        for label in ["Start", "Shut down", "Force stop…"] {
            assert!(ui.find(label).is_err());
        }
        drop(ui);
        let cockpit = app.cockpit.as_mut().unwrap();
        for action in [
            VirtualizationPowerAction::Start,
            VirtualizationPowerAction::Shutdown,
            VirtualizationPowerAction::ForceStop,
        ] {
            assert_eq!(
                cockpit.update(Event::ReviewPower(
                    VirtualizationGuestIdentity {
                        vmid,
                        kind,
                        ..guest()
                    },
                    action
                )),
                Effect::None
            );
            assert!(cockpit.confirmation.is_none());
        }
    }
}

#[test]
fn confirmation_rechecks_freshness_status_identity_node_and_conflicts() {
    for condition in [
        "stale-inventory",
        "failed-inventory",
        "future-inventory",
        "stale-operations",
        "failed-operations",
        "status",
        "template",
        "kind",
        "missing",
        "node",
        "running",
        "unknown",
    ] {
        let mut app = fixture();
        let cockpit = app.cockpit.as_mut().unwrap();
        cockpit.update(Event::ReviewPower(
            guest(),
            VirtualizationPowerAction::Shutdown,
        ));
        let token = cockpit.confirmation.as_ref().unwrap().token();
        match condition {
            "stale-inventory" => cockpit.received_at = Some(unix_now() - 31),
            "failed-inventory" => cockpit.error = Some("API disconnected".into()),
            "future-inventory" => cockpit.received_at = Some(unix_now() + 100),
            "stale-operations" => cockpit.operations_received_at = Some(unix_now() - 31),
            "failed-operations" => cockpit.operations_error = Some("Operations unavailable".into()),
            "status" => cockpit.inventory.as_mut().unwrap().guests[0].status = "stopped".into(),
            "template" => cockpit.inventory.as_mut().unwrap().guests[0].template = true,
            "kind" => {
                cockpit.inventory.as_mut().unwrap().guests[0].kind =
                    VirtualizationGuestKind::Container
            }
            "missing" => cockpit.inventory.as_mut().unwrap().guests.clear(),
            "node" => cockpit.inventory.as_mut().unwrap().nodes[0].status = "offline".into(),
            "running" => cockpit
                .operations
                .push(operation(VirtualizationOperationPhase::Running)),
            "unknown" => cockpit
                .operations
                .push(operation(VirtualizationOperationPhase::Unknown)),
            _ => unreachable!(),
        }
        assert_eq!(
            cockpit.update(Event::Confirm(token)),
            Effect::None,
            "{condition}"
        );
        assert!(cockpit.pending.is_none(), "{condition}");
        assert!(cockpit.power_error.is_some(), "{condition}");
    }
}

#[test]
fn stale_operations_disable_controls_even_with_fresh_inventory_and_after_failure() {
    let _guard = lock();
    for initial in [true, false] {
        let mut app = fixture();
        let cockpit = app.cockpit.as_mut().unwrap();
        if initial {
            cockpit.operations_received_at = None;
        } else {
            receive(cockpit, Err("List connection lost".into()));
        }
        inspect(&mut app, 101);
        let mut ui = simulator(&app, (800.0, 1400.0));
        assert!(ui.find("Power controls paused").is_ok());
        ui.click("Shut down").unwrap();
        ui.click("Force stop…").unwrap();
        assert!(ui.into_messages().next().is_none());
        assert!(app.cockpit.as_ref().unwrap().confirmation.is_none());
    }
}

#[test]
fn power_and_safety_hold_reviews_scroll_to_visible_confirmation_on_guest_details() {
    let _guard = lock();
    for acknowledge in [false, true] {
        let mut app = fixture();
        inspect(&mut app, 101);
        let (event, label) = if acknowledge {
            let unknown = operation(VirtualizationOperationPhase::Unknown);
            app.cockpit
                .as_mut()
                .unwrap()
                .operations
                .push(unknown.clone());
            (
                Event::ReviewAcknowledgement(unknown.id),
                "I inspected Proxmox; release hold",
            )
        } else {
            (
                Event::ReviewPower(guest(), VirtualizationPowerAction::ForceStop),
                "Confirm force stop",
            )
        };
        let task = app.update(Msg::Cockpit(event));
        assert_eq!(task.units(), 1);
        let mut ui = simulator(&app, (800.0, 600.0));
        let confirm = ui.find(label).unwrap();
        let visible = confirm.visible_bounds().unwrap();
        assert!(visible.y >= 0.0 && visible.y + visible.height <= 600.0);
        assert!((visible.height - confirm.bounds().height).abs() < 1.0);
    }
}

#[test]
fn authenticated_rejection_releases_pending_request_and_preserves_the_error() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let request = submit(cockpit, VirtualizationPowerAction::Shutdown);
    let response = Event::submitted(
        cockpit.session,
        request.key,
        Ok(Response::Error {
            message: "This guest already has an active power operation".into(),
        }),
    );
    assert!(matches!(response, Event::Rejected(_, _, _)));
    assert_eq!(cockpit.update(response), Effect::RefreshOperations);
    assert!(cockpit.pending.is_none());
    let error = cockpit.power_error.clone().unwrap();
    receive(cockpit, Ok(vec![]));
    assert_eq!(cockpit.power_error.as_deref(), Some(error.as_str()));
    assert!(
        cockpit
            .power_block_reason(
                &guest(),
                VirtualizationPowerAction::Shutdown,
                unix_now(),
                false
            )
            .is_none()
    );
}

#[test]
fn transport_or_unexpected_replies_remain_uncertain_and_old_rejections_are_ignored() {
    for response in [Err(anyhow::anyhow!("Reply lost")), Ok(Response::Ok)] {
        let mut app = fixture();
        let cockpit = app.cockpit.as_mut().unwrap();
        let request = submit(cockpit, VirtualizationPowerAction::Shutdown);
        let session = cockpit.session;
        let event = Event::submitted(session, request.key, response);
        assert!(matches!(event, Event::Submitted(_, _, Err(_))));
        assert_eq!(cockpit.update(event), Effect::RefreshOperations);
        assert_eq!(cockpit.pending.as_ref().unwrap().request, request);
        assert_eq!(
            cockpit.update(Event::Rejected(
                session,
                request.key,
                "Late rejection".into()
            )),
            Effect::None
        );
        cockpit.reconnect();
        assert_eq!(
            cockpit.update(Event::Rejected(session, request.key, "Old session".into())),
            Effect::None
        );
        assert!(cockpit.pending.is_some());
    }
}

#[test]
fn failed_acknowledgement_stays_visible_after_successful_operation_refresh() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let unknown = operation(VirtualizationOperationPhase::Unknown);
    receive(cockpit, Ok(vec![unknown.clone()]));
    cockpit.update(Event::ReviewAcknowledgement(unknown.id));
    let token = cockpit.confirmation.as_ref().unwrap().token();
    assert_eq!(
        cockpit.update(Event::Confirm(token)),
        Effect::Acknowledge(unknown.id)
    );
    assert_eq!(
        cockpit.update(Event::Acknowledged(
            cockpit.session,
            unknown.id,
            Err("Reply lost".into()),
        )),
        Effect::RefreshOperations
    );
    let error = cockpit.power_error.clone().unwrap();
    receive(cockpit, Ok(vec![unknown]));
    assert_eq!(cockpit.power_error.as_deref(), Some(error.as_str()));
}

#[test]
fn lost_submit_reply_discovers_durable_operation_without_retrying() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let request = submit(cockpit, VirtualizationPowerAction::Shutdown);
    let session = cockpit.session;
    assert_eq!(
        cockpit.update(Event::Submitted(
            session,
            request.key,
            Err("Reply lost".into())
        )),
        Effect::RefreshOperations
    );
    assert_eq!(cockpit.pending.as_ref().unwrap().request, request);
    assert!(!cockpit.pending.as_ref().unwrap().checked_missing);
    assert!(cockpit.operations_stale(unix_now()));
    assert_eq!(
        cockpit.update(Event::RetryRequest(request.key)),
        Effect::None
    );
    assert!(cockpit.confirmation.is_none());
    let mut accepted = operation(VirtualizationOperationPhase::Submitting);
    accepted.id = request.key;
    accepted.task_id = None;
    assert_eq!(receive(cockpit, Ok(vec![accepted.clone()])), Effect::None);
    assert!(cockpit.pending.is_none());
    assert_eq!(cockpit.operations, vec![accepted]);
    assert!(
        cockpit
            .power_block_reason(&guest(), request.action, unix_now(), false)
            .is_some()
    );
}

#[test]
fn explicit_same_key_retry_requires_a_successful_check_and_new_confirmation() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let request = submit(cockpit, VirtualizationPowerAction::ForceStop);
    cockpit.update(Event::Submitted(
        cockpit.session,
        request.key,
        Err("Transport timeout".into()),
    ));
    assert_eq!(
        receive(cockpit, Err("Still unreachable".into())),
        Effect::None
    );
    cockpit.update(Event::RetryRequest(request.key));
    assert!(cockpit.confirmation.is_none());
    assert_eq!(
        cockpit.update(Event::CheckRequest(request.key)),
        Effect::RefreshOperations
    );
    assert_eq!(receive(cockpit, Ok(vec![])), Effect::None);
    assert!(cockpit.pending.as_ref().unwrap().checked_missing);
    cockpit.update(Event::RetryRequest(request.key));
    let token = cockpit.confirmation.as_ref().unwrap().token();
    assert_ne!(token, request.key);
    assert_eq!(cockpit.update(Event::Confirm(request.key)), Effect::None);
    assert_eq!(
        cockpit.update(Event::Confirm(token)),
        Effect::Submit(request.clone())
    );
    assert_eq!(cockpit.pending.as_ref().unwrap().request, request);
    assert_eq!(
        cockpit.update(Event::CheckRequest(request.key)),
        Effect::None
    );
    assert_eq!(
        cockpit.update(Event::RetryRequest(request.key)),
        Effect::None
    );
}

#[test]
fn same_key_retry_is_rechecked_if_guest_state_changes_after_transport_failure() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let request = submit(cockpit, VirtualizationPowerAction::Shutdown);
    cockpit.update(Event::Submitted(
        cockpit.session,
        request.key,
        Err("Timeout".into()),
    ));
    receive(cockpit, Ok(vec![]));
    cockpit.update(Event::RetryRequest(request.key));
    let token = cockpit.confirmation.as_ref().unwrap().token();
    cockpit.inventory.as_mut().unwrap().guests[0].status = "stopped".into();
    assert_eq!(cockpit.update(Event::Confirm(token)), Effect::None);
    assert_eq!(cockpit.pending.as_ref().unwrap().request, request);
    assert!(!cockpit.pending.as_ref().unwrap().in_flight);
}

#[test]
fn async_submission_task_poll_errors_and_failure_remain_durable_history() {
    let _guard = lock();
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let request = submit(cockpit, VirtualizationPowerAction::Shutdown);
    let mut accepted = operation(VirtualizationOperationPhase::Submitting);
    accepted.id = request.key;
    accepted.task_id = None;
    assert_eq!(
        cockpit.update(Event::Submitted(
            cockpit.session,
            request.key,
            Ok(accepted.clone())
        )),
        Effect::RefreshOperations
    );
    assert!(cockpit.pending.is_none());
    assert_eq!(
        cockpit.operations[0].phase,
        VirtualizationOperationPhase::Submitting
    );
    accepted.phase = VirtualizationOperationPhase::Running;
    accepted.task_id = Some("UPID:durable-task".into());
    accepted.error = Some("Poll temporarily unreachable; still running".into());
    receive(cockpit, Ok(vec![accepted.clone()]));
    assert!(cockpit.operations[0].phase.blocks_guest());
    accepted.phase = VirtualizationOperationPhase::Failed;
    accepted.error = Some("Proxmox task failed: guest shutdown timed out".into());
    receive(cockpit, Ok(vec![accepted.clone()]));
    assert!(cockpit.stale(unix_now()));
    assert_eq!(cockpit.operations[0], accepted);
    let mut ui = simulator(&app, (1180.0, 1600.0));
    for label in [
        "Failed",
        "Proxmox task: UPID:durable-task",
        "Proxmox task failed: guest shutdown timed out",
    ] {
        assert!(ui.find(label).is_ok(), "{label}");
    }
    assert!(ui.find("Dismiss").is_err());
}

#[test]
fn guest_metrics_and_status_never_complete_a_running_task() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    receive(
        cockpit,
        Ok(vec![operation(VirtualizationOperationPhase::Running)]),
    );
    let mut inventory = cockpit.inventory.clone().unwrap();
    inventory.guests[0].status = "stopped".into();
    inventory.guests[0].memory_used_bytes = None;
    cockpit.receive(Ok(inventory.clone()), unix_now());
    assert_eq!(
        cockpit.operations[0].phase,
        VirtualizationOperationPhase::Running
    );
    assert!(
        cockpit
            .power_block_reason(
                &guest(),
                VirtualizationPowerAction::Start,
                unix_now(),
                false
            )
            .is_some()
    );
    receive(
        cockpit,
        Ok(vec![operation(VirtualizationOperationPhase::Succeeded)]),
    );
    assert!(cockpit.stale(unix_now()));
    cockpit.receive(Ok(inventory), unix_now());
    assert!(
        cockpit
            .power_block_reason(
                &guest(),
                VirtualizationPowerAction::Start,
                unix_now(),
                false
            )
            .is_none()
    );
}

#[test]
fn list_failures_keep_known_blockers_and_history_until_reconnect() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let running = operation(VirtualizationOperationPhase::Running);
    receive(cockpit, Ok(vec![running.clone()]));
    receive(cockpit, Err("List failed".into()));
    assert_eq!(cockpit.operations, vec![running.clone()]);
    assert!(cockpit.operations_stale(unix_now()));
    receive(cockpit, Ok(vec![]));
    assert_eq!(cockpit.operations, vec![running]);
    assert!(
        cockpit
            .power_block_reason(
                &guest(),
                VirtualizationPowerAction::Shutdown,
                unix_now(),
                false
            )
            .is_some()
    );
}

#[test]
fn old_refresh_and_profile_session_results_cannot_replace_newer_progress() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let (session, generation, revision) = cockpit.begin_operations_refresh();
    let request = submit(cockpit, VirtualizationPowerAction::Shutdown);
    assert_eq!(
        cockpit.update(Event::Operations {
            session,
            generation,
            revision,
            response: Ok(vec![])
        }),
        Effect::RefreshOperations
    );
    assert!(cockpit.pending.as_ref().unwrap().in_flight);
    let mut accepted = operation(VirtualizationOperationPhase::Running);
    accepted.id = request.key;
    cockpit.update(Event::Submitted(session, request.key, Ok(accepted.clone())));
    let (session, generation, revision) = cockpit.begin_operations_refresh();
    cockpit.begin_operations_refresh();
    cockpit.update(Event::Operations {
        session,
        generation,
        revision,
        response: Err("Old failure".into()),
    });
    assert!(cockpit.operations_error.is_none());
    cockpit.reconnect();
    cockpit.update(Event::Operations {
        session,
        generation,
        revision,
        response: Ok(vec![]),
    });
    cockpit.update(Event::Submitted(
        session,
        request.key,
        Err("Old submit failure".into()),
    ));
    assert_eq!(cockpit.operations, vec![accepted]);
    assert!(cockpit.operations_stale(unix_now()));
    assert!(cockpit.pending.is_none());
}

#[test]
fn reconnect_preserves_uncertain_request_key_and_never_automatically_submits() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let request = submit(cockpit, VirtualizationPowerAction::Shutdown);
    let old_session = cockpit.session;
    cockpit.reconnect();
    assert_ne!(cockpit.session, old_session);
    assert_eq!(cockpit.pending.as_ref().unwrap().request, request);
    assert!(!cockpit.pending.as_ref().unwrap().in_flight);
    assert!(cockpit.stale(unix_now()));
    assert!(cockpit.operations_stale(unix_now()));
    let mut accepted = operation(VirtualizationOperationPhase::Unknown);
    accepted.id = request.key;
    assert_eq!(receive(cockpit, Ok(vec![accepted])), Effect::None);
    assert!(cockpit.pending.is_none());
    assert_eq!(
        cockpit.operations[0].phase,
        VirtualizationOperationPhase::Unknown
    );
}

#[test]
fn same_second_late_list_cannot_regress_a_completed_operation() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    receive(
        cockpit,
        Ok(vec![operation(VirtualizationOperationPhase::Succeeded)]),
    );
    receive(
        cockpit,
        Ok(vec![operation(VirtualizationOperationPhase::Running)]),
    );
    assert_eq!(
        cockpit.operations[0].phase,
        VirtualizationOperationPhase::Succeeded
    );
}

#[test]
fn unknown_hold_requires_explicit_independent_inspection_acknowledgement_not_retry() {
    let _guard = lock();
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    receive(
        cockpit,
        Ok(vec![operation(VirtualizationOperationPhase::Unknown)]),
    );
    assert!(
        cockpit
            .power_block_reason(
                &guest(),
                VirtualizationPowerAction::Shutdown,
                unix_now(),
                false
            )
            .is_some()
    );
    let mut ui = simulator(&app, (800.0, 1600.0));
    ui.click("Review safety hold…").unwrap();
    for message in ui.into_messages() {
        let _ = app.update(message);
    }
    let mut ui = simulator(&app, (800.0, 800.0));
    assert!(ui.find("Release Unknown safety hold?").is_ok());
    assert!(ui.find("Independently inspect this VM and its tasks in Proxmox first. Acknowledgement only releases the safety hold; it does not retry the action or verify success.").is_ok());
    drop(ui);
    let cockpit = app.cockpit.as_mut().unwrap();
    let token = cockpit.confirmation.as_ref().unwrap().token();
    assert_eq!(
        cockpit.update(Event::Confirm(token)),
        Effect::Acknowledge(id(810))
    );
    assert!(cockpit.pending.is_none());
    assert_eq!(
        cockpit.update(Event::Acknowledged(
            cockpit.session,
            id(810),
            Err("Lost reply".into())
        )),
        Effect::RefreshOperations
    );
    assert_eq!(
        cockpit.operations[0].phase,
        VirtualizationOperationPhase::Unknown
    );
    assert!(cockpit.operations_stale(unix_now()));
    assert_eq!(
        receive(
            cockpit,
            Ok(vec![operation(VirtualizationOperationPhase::Acknowledged)])
        ),
        Effect::None
    );
    assert!(!cockpit.operations[0].phase.blocks_guest());
    assert!(cockpit.stale(unix_now()));
    assert!(cockpit.pending.is_none());
}

#[test]
fn acknowledgement_confirmation_rechecks_phase_and_operations_freshness() {
    for condition in ["stale", "failed", "changed", "wrong-session"] {
        let mut app = fixture();
        let cockpit = app.cockpit.as_mut().unwrap();
        receive(
            cockpit,
            Ok(vec![operation(VirtualizationOperationPhase::Unknown)]),
        );
        cockpit.update(Event::ReviewAcknowledgement(id(810)));
        let token = cockpit.confirmation.as_ref().unwrap().token();
        match condition {
            "stale" => cockpit.operations_received_at = Some(unix_now() - 31),
            "failed" => {
                receive(cockpit, Err("Offline".into()));
            }
            "changed" => {
                receive(
                    cockpit,
                    Ok(vec![operation(VirtualizationOperationPhase::Acknowledged)]),
                );
            }
            "wrong-session" => cockpit.reconnect(),
            _ => unreachable!(),
        }
        assert_eq!(cockpit.update(Event::Confirm(token)), Effect::None);
        assert!(cockpit.acknowledging.is_none());
    }
}

#[test]
fn restart_recovers_active_operations_and_history_is_selected_guest_scoped() {
    let _guard = lock();
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let mut other = operation(VirtualizationOperationPhase::Succeeded);
    other.id = id(811);
    other.guest.vmid = 102;
    other.task_id = Some("UPID:other-guest".into());
    assert_eq!(
        receive(
            cockpit,
            Ok(vec![
                other,
                operation(VirtualizationOperationPhase::Running)
            ])
        ),
        Effect::None
    );
    assert!(cockpit.pending.is_none());
    assert!(cockpit.confirmation.is_none());
    let mut ui = simulator(&app, (1180.0, 1800.0));
    assert!(ui.find("Proxmox task: UPID:other-guest").is_ok());
    assert!(ui.find("Running · waiting for Proxmox task").is_ok());
    drop(ui);
    inspect(&mut app, 101);
    let mut ui = simulator(&app, (1180.0, 1800.0));
    assert!(ui.find("Proxmox task: UPID:other-guest").is_err());
    assert!(ui.find("Running · waiting for Proxmox task").is_ok());
}

#[test]
fn disconnected_app_cannot_confirm_and_legacy_mutations_remain_ignored() {
    let mut app = fixture();
    let _ = app.update(Msg::Cockpit(Event::ReviewPower(
        guest(),
        VirtualizationPowerAction::Shutdown,
    )));
    let token = app
        .cockpit
        .as_ref()
        .unwrap()
        .confirmation
        .as_ref()
        .unwrap()
        .token();
    let _ = app.update(Msg::Cockpit(Event::Confirm(token)));
    assert!(app.cockpit.as_ref().unwrap().pending.is_none());
    for message in [
        Msg::ForceStop(id(810)),
        Msg::Stop(id(810)),
        Msg::Ssh(id(810), false),
        Msg::Reply(Err("Late legacy reply".into())),
    ] {
        let _ = app.update(message);
    }
    assert!(app.cockpit.as_ref().unwrap().pending.is_none());
    assert!(matches!(
        app.cockpit.as_ref().unwrap().confirmation,
        Some(Confirmation::Power(_))
    ));
    assert!(!app.busy);
}

#[test]
fn successful_acknowledgement_releases_only_the_hold_and_requires_fresh_inventory() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    receive(
        cockpit,
        Ok(vec![operation(VirtualizationOperationPhase::Unknown)]),
    );
    cockpit.update(Event::ReviewAcknowledgement(id(810)));
    let token = cockpit.confirmation.as_ref().unwrap().token();
    assert_eq!(
        cockpit.update(Event::Confirm(token)),
        Effect::Acknowledge(id(810))
    );
    let acknowledged = operation(VirtualizationOperationPhase::Acknowledged);
    assert_eq!(
        cockpit.update(Event::Acknowledged(
            cockpit.session,
            id(810),
            Ok(acknowledged.clone())
        )),
        Effect::RefreshOperations
    );
    assert_eq!(cockpit.operations, vec![acknowledged]);
    assert!(cockpit.pending.is_none());
    assert!(cockpit.stale(unix_now()));
    let inventory = cockpit.inventory.clone().unwrap();
    cockpit.receive(Ok(inventory), unix_now());
    assert!(
        cockpit
            .power_block_reason(
                &guest(),
                VirtualizationPowerAction::Shutdown,
                unix_now(),
                false
            )
            .is_none()
    );
}

#[test]
fn incorrect_submit_payload_and_old_sessions_never_resolve_the_original_request() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let request = submit(cockpit, VirtualizationPowerAction::Shutdown);
    assert_eq!(
        cockpit.update(Event::Submitted(
            Uuid::new_v4(),
            request.key,
            Ok(operation(VirtualizationOperationPhase::Running))
        )),
        Effect::None
    );
    assert!(cockpit.pending.as_ref().unwrap().in_flight);
    let mut incorrect = operation(VirtualizationOperationPhase::Running);
    incorrect.id = request.key;
    incorrect.guest.vmid = 102;
    assert_eq!(
        cockpit.update(Event::Submitted(
            cockpit.session,
            request.key,
            Ok(incorrect)
        )),
        Effect::RefreshOperations
    );
    assert_eq!(cockpit.pending.as_ref().unwrap().request, request);
    assert!(!cockpit.pending.as_ref().unwrap().in_flight);
    assert!(cockpit.operations.is_empty());
}

#[test]
fn new_actions_stay_blocked_while_a_submission_is_in_flight() {
    let mut app = fixture();
    let cockpit = app.cockpit.as_mut().unwrap();
    let request = submit(cockpit, VirtualizationPowerAction::Shutdown);
    for action in [
        VirtualizationPowerAction::Start,
        VirtualizationPowerAction::Shutdown,
        VirtualizationPowerAction::ForceStop,
    ] {
        assert_eq!(
            cockpit.update(Event::ReviewPower(guest(), action)),
            Effect::None
        );
        assert!(cockpit.confirmation.is_none());
        assert_eq!(cockpit.pending.as_ref().unwrap().request, request);
    }
}
