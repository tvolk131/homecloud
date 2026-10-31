use super::*;
use crate::visual_tests::{host_fixture, id, lock, simulator};
const BOOT: &str = "00000000-0000-4000-8000-000000000010";

pub(super) fn fixture() -> App {
    let mut app = host_fixture();
    app.snapshot.as_mut().unwrap().inventory.shutdown_boot_id = Some(BOOT.into());
    app
}
pub(super) fn operation() -> HostShutdownOperation {
    HostShutdownOperation {
        id: id(910),
        actor: id(1),
        boot_id: BOOT.into(),
        revision: 1,
        phase: HostShutdownPhase::Draining,
        vm_ids: vec![id(10), id(11), id(12)],
        remaining: vec![id(10), id(12)],
        poweroff_attempted: false,
        poweroff_requested_at: None,
        error: None,
    }
}
fn panel() -> Panel {
    Panel::ShutdownHost {
        boot_id: BOOT.into(),
        request_key: id(920),
        hostname: "homecloud-01".into(),
    }
}
#[test]
fn shutdown_button_opens_deliberate_confirmation_and_cancel_does_not_submit() {
    let _guard = lock();
    let mut app = fixture();
    app.page = Page::Host;
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Shut down host…").unwrap();
    let message = ui.into_messages().next().unwrap();
    assert!(matches!(message, Msg::Open(Panel::ShutdownHost { .. })));
    let _ = app.update(message);
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Shut down “homecloud-01”?").is_ok());
    let button = ui.find("Shut down host").unwrap();
    assert!(button.bounds().y + button.bounds().height <= 600.0);
    ui.click("Cancel").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::Close]));
    let _ = app.update(messages[0].clone());
    assert!(app.panel.is_none());
    assert!(!app.busy);
    assert!(app.snapshot.as_ref().unwrap().host_shutdown.is_none());
}
#[test]
fn permission_capability_connectivity_and_boot_change_gate_confirmation() {
    let _guard = lock();
    for condition in ["member", "unsupported", "offline", "pending"] {
        let mut app = fixture();
        match condition {
            "member" => app.snapshot.as_mut().unwrap().me.role = Role::User,
            "unsupported" => app.snapshot.as_mut().unwrap().inventory.shutdown_boot_id = None,
            "offline" => app.snapshot_unavailable = true,
            _ => app.snapshot.as_mut().unwrap().host_shutdown = Some(operation()),
        }
        let _ = app.update(Msg::Open(panel()));
        assert!(app.panel.is_none(), "{condition}");
    }
    let mut app = fixture();
    let _ = app.update(Msg::Open(panel()));
    app.snapshot.as_mut().unwrap().inventory.shutdown_boot_id = Some(id(999).to_string());
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Shut down host").unwrap();
    assert!(ui.into_messages().next().is_none());
}
#[test]
fn confirmation_publishes_only_the_explicit_shutdown_action() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::Open(panel()));
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Shut down host").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::ShutdownHost]
    ));
}
#[test]
fn accepted_shutdown_survives_stale_snapshots_and_disconnect_without_claiming_poweroff() {
    let _guard = lock();
    let mut app = fixture();
    let old = app.snapshot.clone().unwrap();
    let _ = app.update(Msg::Reply(Ok(Response::HostShutdownOperation(operation()))));
    let _ = app.update(Msg::Snapshot(0, Ok(Box::new(old.clone()))));
    assert!(app.shutdown_blocks_work());
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Shutting down VMs…").is_ok());
    drop(ui);
    let _ = app.update(Msg::Snapshot(0, Err("Connection closed".into())));
    assert!(app.error.is_none());
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Host unreachable").is_ok());
    assert!(ui.find("Host powered off").is_err());
    drop(ui);
    let mut newer = old;
    newer.inventory.shutdown_boot_id = Some(id(998).to_string());
    let _ = app.update(Msg::Snapshot(0, Ok(Box::new(newer))));
    assert!(!app.shutdown_blocks_work());
}
#[test]
fn recovery_and_workload_controls_follow_the_shutdown_fence() {
    let _guard = lock();
    let mut app = fixture();
    let mut op = operation();
    op.phase = HostShutdownPhase::NeedsReview;
    op.error = Some("Could not shut down devbox: guest did not stop".into());
    app.snapshot.as_mut().unwrap().host_shutdown = Some(op.clone());
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Retry shutdown").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::RetryHostShutdown(_)]
    ));
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Cancel shutdown and resume VMs").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::CancelHostShutdown(_)]
    ));
    app.snapshot
        .as_mut()
        .unwrap()
        .host_shutdown
        .as_mut()
        .unwrap()
        .poweroff_attempted = true;
    assert!(
        simulator(&app, (1180.0, 820.0))
            .find("Cancel shutdown and resume VMs")
            .is_err()
    );
    app.authorization_input = "retained authorization".into();
    let _ = app.update(Msg::SubmitAuthorization(id(200), id(201)));
    let _ = app.update(Msg::SetupApplication(
        id(200),
        ApplicationSetupAction::ConnectT3,
    ));
    let _ = app.update(Msg::InstallApplication(id(10)));
    assert!(app.app_request.is_none());
    assert_eq!(app.authorization_input, "retained authorization");
    app.page = Page::Workspaces;
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("New VM").unwrap();
    assert!(ui.into_messages().next().is_none());
    app.page = Page::Workspace(id(11));
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click(iced::widget::Id::from(format!("vm-lifecycle-{}", id(11))))
        .unwrap();
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn fresh_multi_admin_snapshots_replace_cancelled_receipts_but_older_requests_cannot() {
    let _guard = lock();
    let mut app = fixture();
    app.refresh_generation = 3;
    let mut cancelled = operation();
    cancelled.phase = HostShutdownPhase::Cancelled;
    let _ = app.update(Msg::Reply(Ok(Response::HostShutdownOperation(
        cancelled.clone(),
    ))));
    let mut next = app.snapshot.clone().unwrap();
    let mut started = operation();
    started.id = id(930);
    started.actor = id(2);
    next.host_shutdown = Some(started.clone());
    let _ = app.update(Msg::Snapshot(3, Ok(Box::new(next.clone()))));
    assert_eq!(
        app.snapshot
            .as_ref()
            .unwrap()
            .host_shutdown
            .as_ref()
            .unwrap()
            .id,
        cancelled.id
    );
    app.refresh_generation = 4;
    let _ = app.update(Msg::Snapshot(4, Ok(Box::new(next))));
    assert_eq!(
        app.snapshot
            .as_ref()
            .unwrap()
            .host_shutdown
            .as_ref()
            .unwrap()
            .id,
        started.id
    );
    assert!(app.shutdown_blocks_work());
    assert!(!app.can_shutdown_host());
    let mut older = app.snapshot.clone().unwrap();
    older.host_shutdown = Some(cancelled);
    let _ = app.update(Msg::Snapshot(3, Ok(Box::new(older))));
    assert_eq!(
        app.snapshot
            .as_ref()
            .unwrap()
            .host_shutdown
            .as_ref()
            .unwrap()
            .id,
        started.id
    );
}

#[test]
fn vm_labels_stop_spinning_while_shutdown_is_waiting_for_review() {
    let _guard = lock();
    let mut app = fixture();
    let mut op = operation();
    op.phase = HostShutdownPhase::NeedsReview;
    app.snapshot.as_mut().unwrap().host_shutdown = Some(op);
    let s = app.snapshot.as_ref().unwrap();
    assert!(ui::lifecycle::state(&app, s, &s.vms[0]) == ui::lifecycle::State::Running);
    assert!(ui::lifecycle::state(&app, s, &s.vms[1]) == ui::lifecycle::State::Stopped);
    assert!(ui::lifecycle::state(&app, s, &s.vms[2]) == ui::lifecycle::State::Running);
    app.snapshot
        .as_mut()
        .unwrap()
        .host_shutdown
        .as_mut()
        .unwrap()
        .phase = HostShutdownPhase::Draining;
    let s = app.snapshot.as_ref().unwrap();
    assert!(ui::lifecycle::state(&app, s, &s.vms[0]) == ui::lifecycle::State::Stopping);
    assert!(ui::lifecycle::state(&app, s, &s.vms[2]) == ui::lifecycle::State::Running);
}

#[test]
fn stalled_poweroff_recovery_fits_a_small_window_and_keeps_security_actions_available() {
    let _guard = lock();
    let mut app = fixture();
    let mut op = operation();
    op.phase = HostShutdownPhase::NeedsReview;
    op.poweroff_attempted = true;
    op.error=Some("Power-off was requested, but the host is still reachable after one minute. The request may still be queued. Review the host and explicitly retry; cancellation cannot keep it on.".into());
    app.snapshot.as_mut().unwrap().host_shutdown = Some(op);
    let mut ui = simulator(&app, (800.0, 600.0));
    let retry = ui.find("Retry shutdown").unwrap();
    assert!(retry.bounds().y + retry.bounds().height <= 600.0);
    assert!(ui.find("Cancel shutdown and resume VMs").is_err());
    drop(ui);
    let _ = app.rpc(Request::RevokeDevice {
        peer: "lost-device".into(),
    });
    assert!(app.error.is_none());
}
