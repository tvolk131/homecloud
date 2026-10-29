use super::*;
use crate::visual_tests::{fixture, id};
use create_recovery::{Binding, Outbox, PendingCreate};
use iced::{Settings, Size};
use iced_test::Simulator;
use std::path::Path;
fn binding() -> Binding {
    Binding {
        server: "test-host".into(),
        device: "test-device".into(),
    }
}
fn spec() -> VmSpec {
    VmSpec {
        name: "saved-devbox".into(),
        memory_gib: 2,
        cores: 1,
        disk_gib: 10,
        applications: vec![],
        ssh_public_key:
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                .into(),
    }
}
fn setup() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fixture();
    app.profile = dir.path().join("client.json");
    std::fs::write(&app.profile, b"profile fixture").unwrap();
    app.load_create_outbox(binding()).unwrap();
    (dir, app)
}
fn pending(app: &mut App) -> PendingCreate {
    let pending = app.create_outbox.as_ref().unwrap().prepare(spec()).unwrap();
    app.create_pending = Some(pending.clone());
    app.panel = Some(Panel::Create);
    pending
}
fn operation(p: &PendingCreate) -> Operation {
    Operation {
        id: id(700),
        vm_id: id(701),
        actor: id(1),
        request: Mutation::Create {
            spec: p.spec.clone(),
        },
        phase: "pending".into(),
        error: None,
        progress: None,
    }
}
fn receive(app: &mut App, p: &PendingCreate, r: Result<Response, String>) {
    app.busy = true;
    app.create_inflight = Some(p.key);
    let _ = app.update(Msg::CreateReply(p.key, r));
}
#[test]
fn lost_response_and_app_restart_preserve_original_key_and_payload_even_at_full_quota() {
    let (dir, mut app) = setup();
    let saved = pending(&mut app);
    let request = serde_json::to_value(saved.request()).unwrap();
    receive(&mut app, &saved, Err("Response lost".into()));
    assert!(app.create_pending.is_some());
    let _ = app.update(Msg::Edit(Field::Name, "changed".into()));
    let _ = app.update(Msg::ResourceValue(Field::Ram, 8));
    let _ = app.update(Msg::ToggleT3);
    let _ = app.update(Msg::Close);
    assert!(app.panel.is_none());
    drop(app);
    let mut app = fixture();
    app.profile = dir.path().join("client.json");
    app.load_create_outbox(binding()).unwrap();
    assert!(matches!(app.panel, Some(Panel::Create)));
    app.snapshot.as_mut().unwrap().me.grant = Resources::default();
    app.snapshot_unavailable = true;
    let restored = app.create_pending.clone().unwrap();
    assert_eq!(serde_json::to_value(restored.request()).unwrap(), request);
    receive(
        &mut app,
        &restored,
        Ok(Response::Operation(operation(&restored))),
    );
    assert!(app.create_pending.is_none());
    assert!(
        app.create_outbox
            .as_ref()
            .unwrap()
            .load()
            .unwrap()
            .is_none()
    );
    assert_eq!(app.accepted_vm_ops.len(), 1);
    let next = app.create_outbox.as_ref().unwrap().prepare(spec()).unwrap();
    assert_ne!(next.key, saved.key);
}
#[test]
fn generic_server_errors_remain_uncertain_but_committed_rejections_allow_amending() {
    let (_dir, mut app) = setup();
    let saved = pending(&mut app);
    receive(
        &mut app,
        &saved,
        Ok(Response::Error {
            message: "Device disabled".into(),
        }),
    );
    assert!(app.create_pending.is_some());
    receive(
        &mut app,
        &saved,
        Ok(Response::CreateRejected {
            idempotency_key: saved.key,
            message: "Quota exceeded".into(),
        }),
    );
    assert!(app.create_pending.is_none());
    assert_eq!(app.get(Field::Name), saved.spec.name);
    assert_eq!(app.error.as_deref(), Some("Quota exceeded"));
    let _ = app.update(Msg::Close);
    assert!(app.displayed_create().is_none());
    let mut changed = spec();
    changed.memory_gib = 1;
    let next = app
        .create_outbox
        .as_ref()
        .unwrap()
        .prepare(changed)
        .unwrap();
    assert_ne!(next.key, saved.key);
    app.create_pending = Some(next.clone());
    app.panel = Some(Panel::EditWorkspace(id(10)));
    app.fields[Field::Name as usize] = "another-vm-edit".into();
    receive(
        &mut app,
        &next,
        Ok(Response::CreateRejected {
            idempotency_key: next.key,
            message: "rejected after navigation".into(),
        }),
    );
    assert_eq!(app.get(Field::Name), "another-vm-edit");
    assert!(matches!(app.panel, Some(Panel::EditWorkspace(_))));
}
#[test]
fn late_responses_and_changed_payloads_cannot_clear_a_different_saved_request() {
    let (_dir, mut app) = setup();
    let saved = pending(&mut app);
    assert!(app.create_outbox.as_ref().unwrap().prepare(spec()).is_err());
    app.create_inflight = Some(saved.key);
    app.busy = true;
    let _ = app.update(Msg::CreateReply(
        id(999),
        Ok(Response::Operation(operation(&saved))),
    ));
    assert!(app.busy);
    assert_eq!(app.create_inflight, Some(saved.key));
    let mut wrong = operation(&saved);
    if let Mutation::Create { spec } = &mut wrong.request {
        spec.memory_gib = 8;
    }
    receive(&mut app, &saved, Ok(Response::Operation(wrong)));
    assert!(app.create_pending.is_some());
    receive(
        &mut app,
        &saved,
        Ok(Response::CreateRejected {
            idempotency_key: id(999),
            message: "wrong receipt".into(),
        }),
    );
    assert!(app.create_pending.is_some());
}
#[test]
fn corrupt_mismatched_or_locked_journals_fail_closed_without_overwriting_saved_request() {
    let (_dir, app) = setup();
    let outbox = app.create_outbox.as_ref().unwrap();
    let saved = outbox.prepare(spec()).unwrap();
    assert!(Outbox::open(&app.profile, binding()).is_err());
    let profile = app.profile.clone();
    drop(app);
    let wrong = Binding {
        server: "different-host".into(),
        ..binding()
    };
    assert!(Outbox::open(&profile, wrong).is_err());
    let outbox = Outbox::open(&profile, binding()).unwrap();
    assert_eq!(outbox.load().unwrap().unwrap().key, saved.key);
    let mut journal = profile.as_os_str().to_owned();
    journal.push(".create-request.json");
    std::fs::write(Path::new(&journal), b"incomplete JSON").unwrap();
    assert!(outbox.load().is_err());
    assert!(outbox.prepare(spec()).is_err());
    assert_eq!(
        std::fs::read(Path::new(&journal)).unwrap(),
        b"incomplete JSON"
    );
}
#[test]
fn lost_acceptance_end_to_end_replays_one_vm_after_both_client_and_store_reopen() {
    use homecloud_store::{CreateOutcome, Limits, Store};
    let (dir, mut app) = setup();
    let limits = Limits {
        capacity: Resources {
            memory_bytes: 2 * GIB,
            cpu_cores: 1,
            disk_bytes: 12 * GIB,
        },
        core_groups: vec![vec![2, 8]],
        disk_overhead_gib_per_vm: 2,
    };
    let store = Store::open(dir.path().join("server.db"), limits.clone()).unwrap();
    let (user, token) = store.bootstrap("Owner").unwrap();
    store.enroll(&token, "device").unwrap();
    store
        .grant("device", user.id, limits.capacity, false)
        .unwrap();
    let saved = pending(&mut app);
    let first = match store
        .submit_create("device", saved.key, saved.spec.clone())
        .unwrap()
    {
        CreateOutcome::Accepted(op) => op,
        _ => panic!(),
    };
    // Deliberately discard the accepted response. No client-side acknowledgement.
    drop(app);
    drop(store);
    let mut app = fixture();
    app.profile = dir.path().join("client.json");
    app.load_create_outbox(binding()).unwrap();
    let store = Store::open(dir.path().join("server.db"), limits.clone()).unwrap();
    let replay = app.create_pending.clone().unwrap();
    let op = match store
        .submit_create("device", replay.key, replay.spec.clone())
        .unwrap()
    {
        CreateOutcome::Accepted(op) => op,
        _ => panic!(),
    };
    assert_eq!((op.id, op.vm_id), (first.id, first.vm_id));
    receive(&mut app, &replay, Ok(Response::Operation(op)));
    assert!(app.create_pending.is_none());
    assert_eq!(store.vms().unwrap().len(), 1);
    assert_eq!(store.users().unwrap()[0].committed, limits.capacity);
}
#[test]
fn recovery_dialog_is_actionable_at_full_quota_and_retains_its_exit_body() {
    let (_dir, mut app) = setup();
    let saved = pending(&mut app);
    app.snapshot.as_mut().unwrap().me.grant = Resources::default();
    app.snapshot_unavailable = true;
    for size in [(1180.0, 820.0), (800.0, 600.0)] {
        let mut ui = Simulator::with_size(
            Settings {
                default_font: iced_m3::fonts::REGULAR,
                fonts: vec![iced_m3::fonts::ROBOTO.into()],
                ..Default::default()
            },
            Size::from(size),
            app.view(),
        );
        ui.click("Retry request").unwrap();
        assert!(matches!(
            ui.into_messages().collect::<Vec<_>>().as_slice(),
            [Msg::Create]
        ));
    }
    receive(&mut app, &saved, Ok(Response::Operation(operation(&saved))));
    assert!(app.panel.is_none());
    assert!(app.displayed_create().is_some());
    let _ = app.update(Msg::Open(Panel::Create));
    assert!(app.displayed_create().is_none());
    // Queued Create events after acknowledgement/dismissal cannot create another VM.
    let _ = app.update(Msg::Close);
    assert!(app.displayed_create().is_none());
    let _ = app.update(Msg::Create);
    assert!(app.create_pending.is_none());
}

#[test]
fn recovery_dialog_native_screenshots() {
    let _guard = crate::visual_tests::lock();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let baseline = root.join("tests/screenshots");
    let artifacts = root.join("../../target/ui-screenshots");
    std::fs::create_dir_all(&artifacts).unwrap();
    let update = std::env::var("UPDATE_SCREENSHOTS").as_deref() == Ok("1");
    for (name, size, checking) in [
        ("create-recovery", (1180.0, 820.0), false),
        ("create-recovery-small", (800.0, 600.0), false),
        ("create-recovery-checking", (800.0, 600.0), true),
    ] {
        let (_dir, mut app) = setup();
        let saved = pending(&mut app);
        app.reduced_motion = true;
        if checking {
            app.busy = true;
            app.create_inflight = Some(saved.key);
        }
        app.snapshot.as_mut().unwrap().me.grant = Resources::default();
        let mut rendered = crate::visual_tests::simulator(&app, size);
        let snapshot = rendered.snapshot(&app.theme()).unwrap();
        let actual = artifacts.join(format!("{name}-tiny-skia.png"));
        if actual.exists() {
            std::fs::remove_file(&actual).unwrap();
        }
        assert!(
            snapshot
                .matches_image(artifacts.join(format!("{name}.png")))
                .unwrap()
        );
        let expected = baseline.join(format!("{name}-tiny-skia.png"));
        if update && expected.exists() {
            std::fs::remove_file(&expected).unwrap();
        }
        assert!(
            expected.exists() || update,
            "Missing recovery screenshot baseline"
        );
        assert!(
            snapshot
                .matches_image(baseline.join(format!("{name}.png")))
                .unwrap(),
            "{name}"
        );
    }
}
