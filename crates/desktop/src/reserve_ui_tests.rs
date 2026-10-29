use super::*;
use crate::visual_tests::{host_fixture, id, lock, simulator};

pub(super) fn fixture() -> App {
    let mut app = host_fixture();
    let s = app.snapshot.as_mut().unwrap();
    s.me.grant.cpu_cores = 2;
    s.users[0] = s.me.clone();
    let protected = s.users.iter().fold(Resources::default(), |sum, u| {
        sum.checked_add(u.grant.max(u.committed)).unwrap()
    });
    s.host_reserves = Some(HostReserveView {
        memory_operation: None,
        memory: None,
        active: HostReserves::default(),
        operation: None,
        envelope: s.capacity,
        protected_backing: protected,
        base_pool_free_bytes: 30 * GIB,
        can_discard: false,
        minimum_reserves: Resources::default(),
        arc: None,
        memory_policy: None,
    });
    app
}
pub(super) fn operation() -> HostReserveOperation {
    HostReserveOperation {
        id: id(900),
        actor: id(1),
        request_key: id(901),
        before: HostReserves::default(),
        target: HostReserves {
            revision: 1,
            additional: Resources {
                memory_bytes: 2 * GIB,
                cpu_cores: 1,
                disk_bytes: 100 * GIB,
            },
            core_groups: vec![vec![3, 9]],
            arc: None,
        },
        phase: ReservePhase::Pending,
        error: None,
    }
}
#[test]
fn reserve_sliders_use_only_unpromised_capacity_and_keep_stable_draft_ranges() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::Open(Panel::HostReserves));
    let bounds = app.reserve_draft.as_ref().unwrap().bounds;
    assert_eq!((bounds[0].max, bounds[1].max, bounds[2].max), (4, 1, 5120));
    let _ = app.update(Msg::ResourceValue(Field::ReserveMemory, 2));
    let _ = app.update(Msg::ResourceValue(Field::ReserveCores, 1));
    let _ = app.update(Msg::ResourceValue(Field::ReserveDisk, 100));
    assert_eq!(
        app.reserve_draft.as_ref().unwrap().values(&app).unwrap(),
        operation().target.additional
    );
    app.snapshot
        .as_mut()
        .unwrap()
        .host_reserves
        .as_mut()
        .unwrap()
        .protected_backing
        .memory_bytes = 24 * GIB;
    assert_eq!(app.reserve_draft.as_ref().unwrap().bounds[0].max, 4);
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Save reserves").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::SaveHostReserves]
    ));
    let _ = app.update(Msg::Edit(Field::ReserveMemory, "5".into()));
    assert!(app.reserve_draft.as_ref().unwrap().values(&app).is_err());
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Save reserves").unwrap();
    assert!(ui.into_messages().next().is_none());
    let _ = app.update(Msg::Close);
    let _ = app.update(Msg::Open(Panel::HostReserves));
    assert_eq!(app.reserve_draft.as_ref().unwrap().bounds[0].max, 0);
}
#[test]
fn reserve_editor_requires_admin_and_host_capability() {
    let _guard = lock();
    let mut app = fixture();
    app.snapshot.as_mut().unwrap().me.role = Role::User;
    let _ = app.update(Msg::Open(Panel::HostReserves));
    assert!(!matches!(app.panel, Some(Panel::HostReserves)));
    let mut app = fixture();
    app.snapshot.as_mut().unwrap().host_reserves = None;
    let _ = app.update(Msg::Open(Panel::HostReserves));
    assert!(!matches!(app.panel, Some(Panel::HostReserves)));
}
#[test]
fn accepted_reserve_progress_survives_stale_snapshots_and_announces_confirmation() {
    let _guard = lock();
    let mut app = fixture();
    let old = app.snapshot.clone().unwrap();
    let op = operation();
    let _ = app.update(Msg::Reply(Ok(Response::HostReserveOperation(op.clone()))));
    assert_eq!(app.snapshot.as_ref().unwrap().capacity.cpu_cores, 3);
    let _ = app.update(Msg::Snapshot(Ok(Box::new(old.clone()))));
    assert_eq!(
        app.snapshot
            .as_ref()
            .unwrap()
            .host_reserves
            .as_ref()
            .unwrap()
            .operation
            .as_ref()
            .unwrap()
            .id,
        op.id
    );
    assert_eq!(app.snapshot.as_ref().unwrap().capacity.cpu_cores, 3);
    let mut missing = old.clone();
    missing.host_reserves = None;
    let _ = app.update(Msg::Snapshot(Ok(Box::new(missing))));
    assert_eq!(app.snapshot.as_ref().unwrap().capacity.cpu_cores, 3);
    assert!(app.snapshot.as_ref().unwrap().host_reserves.is_some());
    let mut completed = old;
    let view = completed.host_reserves.as_mut().unwrap();
    view.active = op.target.clone();
    let mut done = op;
    done.phase = ReservePhase::Succeeded;
    view.operation = Some(done);
    completed.capacity = view.envelope.checked_sub(view.active.additional).unwrap();
    let _ = app.update(Msg::Snapshot(Ok(Box::new(completed))));
    assert!(app.accepted_reserve.is_none());
    assert_eq!(app.notice, "Host reserves updated.");
}
#[test]
fn recovery_buttons_target_the_operation_and_discard_is_only_shown_when_safe() {
    let _guard = lock();
    let mut app = fixture();
    let mut op = operation();
    op.phase = ReservePhase::NeedsReview;
    op.error = Some("CPU fence update was interrupted.".into());
    let view = app
        .snapshot
        .as_mut()
        .unwrap()
        .host_reserves
        .as_mut()
        .unwrap();
    view.operation = Some(op.clone());
    view.can_discard = false;
    let mut ui = simulator(&app, (1180.0, 1040.0));
    crate::visual_tests::scroll_bottom(&mut ui, (1180.0, 1040.0));
    assert!(ui.find("Discard unapplied change").is_err());
    ui.click("Retry reserve change").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),[Msg::RetryHostReserves(id)] if *id==op.id)
    );
    app.snapshot
        .as_mut()
        .unwrap()
        .host_reserves
        .as_mut()
        .unwrap()
        .can_discard = true;
    let mut ui = simulator(&app, (1180.0, 1040.0));
    crate::visual_tests::scroll_bottom(&mut ui, (1180.0, 1040.0));
    ui.click("Discard unapplied change").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),[Msg::DiscardHostReserves(id)] if *id==op.id)
    );
}
#[test]
fn closing_reserve_editor_retains_its_slider_geometry_during_exit() {
    let _guard = lock();
    let mut app = fixture();
    let _ = app.update(Msg::Open(Panel::HostReserves));
    let geometry = |app: &App| {
        let mut ui = crate::dialog_tests::retained(app, (800.0, 600.0));
        [
            "resource-slider-memory",
            "resource-slider-cores",
            "resource-slider-disk",
        ]
        .map(|name| ui.find(iced::widget::Id::new(name)).unwrap().bounds())
    };
    let before = geometry(&app);
    let _ = app.update(Msg::Close);
    assert_eq!(geometry(&app), before);
    let values = app.fields.clone();
    let _ = app.update(Msg::ResourceValue(Field::ReserveMemory, 1));
    assert_eq!(app.fields, values);
}

pub(super) fn memory_fixture() -> App {
    let mut app = fixture();
    let s = app.snapshot.as_mut().unwrap();
    s.inventory.memory_bytes = 28 * GIB;
    s.inventory.physical_cores = 5;
    let view = s.host_reserves.as_mut().unwrap();
    view.minimum_reserves = Resources {
        memory_bytes: 4 * GIB,
        cpu_cores: 1,
        disk_bytes: 30 * GIB,
    };
    view.active.additional = Resources {
        memory_bytes: 4 * GIB,
        cpu_cores: 1,
        disk_bytes: 0,
    };
    view.active.arc = Some(ArcMode::Custom { max_bytes: 2 * GIB });
    view.memory_policy = Some(HostMemoryPolicy {
        operating_floor_bytes: 2 * GIB,
        recommended_arc_bytes: 2 * GIB,
        minimum_system_bytes: 4 * GIB,
    });
    view.arc = Some(ArcTelemetry {
        size_bytes: 3 * GIB,
        minimum_bytes: GIB / 2,
        maximum_bytes: 2 * GIB,
        parameter_bytes: 2 * GIB,
        automatic_maximum_bytes: Some(27 * GIB),
        version: "2.4.1".into(),
    });
    view.arc.as_mut().unwrap().automatic_maximum_bytes = Some(27 * GIB);
    s.capacity = view.envelope.checked_sub(view.active.additional).unwrap();
    app
}
#[test]
fn total_reserve_editor_can_release_the_original_host_margin_and_cache_is_independent() {
    let _guard = lock();
    let mut app = memory_fixture();
    let _ = app.update(Msg::Open(Panel::HostReserves));
    assert_eq!(app.get(Field::ReserveMemory), "8");
    assert_eq!(app.reserve_draft.as_ref().unwrap().bounds[0].min, 4);
    let _ = app.update(Msg::ResourceValue(Field::ReserveMemory, 4));
    assert_eq!(
        app.reserve_draft
            .as_ref()
            .unwrap()
            .values(&app)
            .unwrap()
            .memory_bytes,
        4 * GIB
    );
    let bounds = app.reserve_draft.as_ref().unwrap().bounds;
    let _ = app.update(Msg::ReserveAutomatic(true));
    assert_eq!(
        app.reserve_draft.as_ref().unwrap().arc(&app).unwrap(),
        Some(ArcMode::Automatic)
    );
    assert_eq!(
        app.reserve_draft.as_ref().unwrap().bounds[0].max,
        bounds[0].max
    );
    let _ = app.update(Msg::ReserveAutomatic(false));
    let _ = app.update(Msg::ResourceValue(Field::ArcLimit, 2));
    assert_eq!(
        app.reserve_draft.as_ref().unwrap().arc(&app).unwrap(),
        Some(ArcMode::Custom { max_bytes: 2 * GIB })
    );
    let _ = app.update(Msg::Edit(Field::ReserveMemory, "3".into()));
    assert!(app.reserve_draft.as_ref().unwrap().values(&app).is_err());
}

#[test]
fn native_zero_is_not_homecloud_automatic_and_caps_cannot_exceed_reserved_memory() {
    let _guard = lock();
    let mut app = memory_fixture();
    let view = app
        .snapshot
        .as_mut()
        .unwrap()
        .host_reserves
        .as_mut()
        .unwrap();
    view.active.arc = None;
    let arc = view.arc.as_mut().unwrap();
    arc.parameter_bytes = 0;
    arc.maximum_bytes = 27 * GIB;
    arc.automatic_maximum_bytes = None; // Native defaults need not be known for our reviewed policy.
    let _ = app.update(Msg::Open(Panel::HostReserves));
    assert!(!app.reserve_draft.as_ref().unwrap().automatic);
    assert!(app.reserve_draft.as_ref().unwrap().arc(&app).is_err());
    let _ = app.update(Msg::ReserveAutomatic(true));
    assert_eq!(
        app.reserve_draft.as_ref().unwrap().arc(&app).unwrap(),
        Some(ArcMode::Automatic)
    );
    let _ = app.update(Msg::ReserveAutomatic(false));
    let _ = app.update(Msg::ResourceValue(Field::ReserveMemory, 4));
    let _ = app.update(Msg::Edit(Field::ArcLimit, "3".into()));
    assert!(app.reserve_draft.as_ref().unwrap().arc(&app).is_err());
    let _ = app.update(Msg::ResourceValue(Field::ReserveMemory, 5));
    assert_eq!(
        app.reserve_draft.as_ref().unwrap().arc(&app).unwrap(),
        Some(ArcMode::Custom { max_bytes: 3 * GIB })
    );
}

#[test]
fn memory_review_keeps_the_opened_target_and_blocks_a_changed_attestation() {
    let _guard = lock();
    let mut app = memory_fixture();
    app.snapshot
        .as_mut()
        .unwrap()
        .host_reserves
        .as_mut()
        .unwrap()
        .memory = Some(MemoryCapacityStatus {
        revision: 7,
        reviewed_bytes: 28 * GIB,
        safe_bytes: 60 * GIB,
        physical_bytes: 64 * GIB,
        minimum_system_bytes: 4 * GIB,
        boot_id: id(5).to_string(),
        observed_at: unix_now(),
        arc_ready: true,
        error: None,
    });
    let _ = app.update(Msg::Open(Panel::ReviewMemoryCapacity));
    assert_eq!(app.memory_review.as_ref().unwrap().safe_bytes, 60 * GIB);
    app.snapshot
        .as_mut()
        .unwrap()
        .host_reserves
        .as_mut()
        .unwrap()
        .memory
        .as_mut()
        .unwrap()
        .safe_bytes = 40 * GIB;
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click("Adopt capacity").unwrap();
    assert!(ui.into_messages().next().is_none());
    let _ = app.update(Msg::AdoptMemoryCapacity);
    assert!(app.error.as_ref().unwrap().contains("changed"));
    assert_eq!(app.memory_review.as_ref().unwrap().safe_bytes, 60 * GIB);
}
#[test]
fn workload_choices_recalculate_backing_and_return_to_ordinary_vm_minimums() {
    let _guard = lock();
    let mut app = crate::visual_tests::fixture();
    let _ = app.update(Msg::Open(Panel::Create));
    let ordinary = resources::FormBounds::current(&app).unwrap();
    let _ = app.update(Msg::SelectWorkload(Some(ApplicationKind::HomecloudHost)));
    let host = resources::FormBounds::current(&app).unwrap();
    assert_eq!((host.memory.min, host.cores.min, host.disk.min), (8, 2, 32));
    assert_eq!(host.disk.max, ordinary.disk.max - 32);
    let _ = app.update(Msg::SelectWorkload(Some(ApplicationKind::T3)));
    let t3 = resources::FormBounds::current(&app).unwrap();
    assert_eq!((t3.memory.min, t3.cores.min, t3.disk.min), (1, 1, 10));
    assert!(!app.install_managed_host && app.install_t3);
    let _ = app.update(Msg::SelectWorkload(None));
    assert!(!app.install_managed_host && !app.install_t3);
}

#[test]
fn managed_host_card_is_identified_even_before_application_inspection() {
    let _guard = lock();
    let mut app = crate::visual_tests::fixture();
    app.snapshot.as_mut().unwrap().vms[0].spec.applications = vec![ApplicationKind::HomecloudHost];
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("Ubuntu · Homecloud host").is_ok());
}

#[test]
fn unchanged_cache_policy_can_be_explicitly_reapplied_after_effective_drift() {
    let _guard = lock();
    let mut app = memory_fixture();
    app.snapshot
        .as_mut()
        .unwrap()
        .host_reserves
        .as_mut()
        .unwrap()
        .memory = Some(MemoryCapacityStatus {
        revision: 0,
        reviewed_bytes: 28 * GIB,
        safe_bytes: 28 * GIB,
        physical_bytes: 32 * GIB,
        minimum_system_bytes: 4 * GIB,
        boot_id: id(5).to_string(),
        observed_at: unix_now(),
        arc_ready: false,
        error: Some("Effective cache cap drifted".into()),
    });
    let _ = app.update(Msg::Open(Panel::HostReserves));
    let mut ui = simulator(&app, (1180.0, 1040.0));
    ui.click("Reapply cache policy").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::SaveHostReserves]
    ));
}
