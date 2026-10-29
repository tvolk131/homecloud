use super::*;
use crate::visual_tests::{fixture as ordinary, id, lock, simulator};

pub(super) fn fixture(condition: MemoryCondition) -> App {
    let mut app = ordinary();
    let s = app.snapshot.as_mut().unwrap();
    for vm in &mut s.vms {
        vm.desired = Lifecycle::Running;
        vm.observed = "stopped".into();
        vm.resources.memory_bytes = 4 * GIB;
        vm.resources.cpu_cores = 1;
    }
    s.vms[1].owner = id(2);
    s.me.grant.cpu_cores = 2;
    s.me.grant.disk_bytes = 160 * GIB;
    s.me.committed.disk_bytes = 42 * GIB;
    s.users[0] = s.me.clone();
    s.users[1].committed = Resources {
        memory_bytes: 4 * GIB,
        cpu_cores: 1,
        disk_bytes: 42 * GIB,
    };
    let budget = if condition == MemoryCondition::PromisesExceedCapacity {
        10 * GIB
    } else {
        6 * GIB
    };
    s.capacity.memory_bytes = budget;
    s.memory_health = Some(HostMemoryHealth {
        condition,
        details: Some(MemoryHealthDetails {
            capacity_bytes: budget,
            promised_bytes: 24 * GIB,
            held_bytes: 8 * GIB,
        }),
    });
    s.host_reserves = Some(HostReserveView {
        memory_operation: None,
        memory: Some(MemoryCapacityStatus {
            revision: 0,
            reviewed_bytes: 28 * GIB,
            safe_bytes: budget,
            physical_bytes: budget + 4 * GIB,
            minimum_system_bytes: 4 * GIB,
            boot_id: id(5).to_string(),
            observed_at: unix_now(),
            arc_ready: condition != MemoryCondition::ArcNotEnforced,
            error: None,
        }),
        active: HostReserves {
            arc: Some(ArcMode::Custom { max_bytes: 2 * GIB }),
            ..HostReserves::default()
        },
        operation: None,
        envelope: Resources {
            memory_bytes: 28 * GIB,
            ..s.capacity
        },
        protected_backing: s.users.iter().fold(Resources::default(), |sum, u| {
            sum.checked_add(u.grant.max(u.committed)).unwrap()
        }),
        base_pool_free_bytes: 30 * GIB,
        can_discard: false,
        minimum_reserves: Resources {
            memory_bytes: 4 * GIB,
            cpu_cores: 1,
            disk_bytes: 30 * GIB,
        },
        arc: Some(ArcTelemetry {
            size_bytes: GIB,
            minimum_bytes: GIB / 2,
            maximum_bytes: 2 * GIB,
            parameter_bytes: 2 * GIB,
            automatic_maximum_bytes: Some(9 * GIB),
            version: "2.4.1".into(),
        }),
        memory_policy: Some(HostMemoryPolicy {
            operating_floor_bytes: 2 * GIB,
            recommended_arc_bytes: 2 * GIB,
            minimum_system_bytes: 4 * GIB,
        }),
    });
    app
}
#[test]
fn blocked_start_is_not_a_spinner_and_stop_still_requires_confirmation() {
    let _guard = lock();
    let mut app = fixture(MemoryCondition::AllocationsExceedCapacity);
    let s = app.snapshot.as_ref().unwrap();
    assert!(matches!(
        ui::lifecycle::state(&app, s, &s.vms[0]),
        ui::lifecycle::State::Blocked
    ));
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Review host").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::ReviewHostMemory]
    ));
    let _ = app.update(Msg::ReviewHostMemory);
    assert_eq!(app.page, Page::Host);
    assert_eq!(app.host_tab, HostTab::Overview);
    let _ = app.update(Msg::OpenWorkspace(id(10)));
    let mut ui = simulator(&app, (1180.0, 1040.0));
    ui.click("Stop and release compute…").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(),[Msg::Open(Panel::Stop(vm))] if *vm==id(10)));
    let _ = app.update(messages[0].clone());
    let mut ui = simulator(&app, (1180.0, 1040.0));
    ui.click("Cancel").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Close]
    ));
}
#[test]
fn memory_warning_survives_navigation_notices_and_offline_state_until_resolved() {
    let _guard = lock();
    let mut app = fixture(MemoryCondition::ArcNotEnforced);
    for page in [
        Page::Workspaces,
        Page::Hosts,
        Page::People,
        Page::Activity,
        Page::Settings,
    ] {
        let _ = app.update(Msg::Navigate(page));
        let _ = app.update(Msg::DismissNotice);
        let mut ui = simulator(&app, (800.0, 600.0));
        assert!(ui.find("ZFS cache policy needs repair").is_ok());
    }
    app.snapshot_unavailable = true;
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(
        ui.find("Last verified memory warning. Reconnect to check whether recovery has completed.")
            .is_ok()
    );
    drop(ui);
    app.snapshot_unavailable = false;
    app.snapshot
        .as_mut()
        .unwrap()
        .memory_health
        .as_mut()
        .unwrap()
        .condition = MemoryCondition::Healthy;
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("ZFS cache policy needs repair").is_err());
}
#[test]
fn stopped_vm_start_is_disabled_without_hiding_stop_or_member_guidance() {
    let _guard = lock();
    let mut app = fixture(MemoryCondition::PromisesExceedCapacity);
    let s = app.snapshot.as_mut().unwrap();
    let vm = &mut s.vms[0];
    vm.desired = Lifecycle::Stopped;
    vm.resources = vm.resources.disk_only();
    let mut ui = simulator(&app, (1180.0, 1040.0));
    ui.click(iced::widget::Id::from(format!("vm-lifecycle-{}", id(10))))
        .unwrap();
    assert!(ui.into_messages().next().is_none());
    app.snapshot.as_mut().unwrap().me.role = Role::User;
    app.snapshot
        .as_mut()
        .unwrap()
        .memory_health
        .as_mut()
        .unwrap()
        .details = None;
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Ask your administrator").is_ok());
    assert!(ui.find("Review host").is_err());
    drop(ui);
    let _ = app.update(Msg::ReviewHostMemory);
    assert_ne!(app.page, Page::Host);
}

#[test]
fn unavailable_power_observation_is_not_presented_as_a_confirmed_blocked_boot() {
    let _guard = lock();
    let mut app = fixture(MemoryCondition::AllocationsExceedCapacity);
    app.snapshot.as_mut().unwrap().vms[0].observed = "unavailable".into();
    let s = app.snapshot.as_ref().unwrap();
    assert!(!s.memory_health.as_ref().unwrap().waiting(&s.vms[0]));
    assert!(matches!(
        ui::lifecycle::state(&app, s, &s.vms[0]),
        ui::lifecycle::State::Unavailable
    ));
    let _ = app.update(Msg::OpenWorkspace(id(10)));
    let mut ui = simulator(&app, (1180.0, 1040.0));
    assert!(ui.find("VM starting").is_err());
}

#[test]
fn shortage_reserve_editor_uses_physical_budget_and_still_allows_cache_repair() {
    let _guard = lock();
    let mut app = fixture(MemoryCondition::ArcNotEnforced);
    let _ = app.update(Msg::Open(Panel::HostReserves));
    let draft = app.reserve_draft.as_ref().unwrap();
    assert_eq!(draft.envelope.memory_bytes, 6 * GIB);
    assert_eq!(draft.bounds[0].max, 4);
    assert!(draft.values(&app).is_ok());
    let mut ui = simulator(&app, (1180.0, 1040.0));
    ui.click("Reapply cache policy").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::SaveHostReserves]
    ));
    let _ = app.update(Msg::Edit(Field::ReserveMemory, "5".into()));
    assert!(app.reserve_draft.as_ref().unwrap().values(&app).is_err());
}

#[test]
fn repairing_unmanaged_arc_drift_proposes_a_concrete_backed_limit() {
    let _guard = lock();
    let mut app = fixture(MemoryCondition::ArcNotEnforced);
    let view = app
        .snapshot
        .as_mut()
        .unwrap()
        .host_reserves
        .as_mut()
        .unwrap();
    view.active.arc = None;
    view.arc.as_mut().unwrap().maximum_bytes = 3 * GIB;
    let _ = app.update(Msg::Open(Panel::HostReserves));
    assert_eq!(app.get(Field::ArcLimit), "2");
    assert_eq!(
        app.reserve_draft.as_ref().unwrap().arc(&app).unwrap(),
        Some(ArcMode::Custom { max_bytes: 2 * GIB })
    );
    let mut ui = simulator(&app, (1180.0, 1040.0));
    ui.click("Reapply cache policy").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::SaveHostReserves]
    ));
}
