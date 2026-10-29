use super::*;
use crate::visual_tests::{host_fixture, id, lock, simulator};

#[test]
fn host_directory_opens_details_and_back_returns_to_the_directory() {
    let _guard = lock();
    let mut app = host_fixture();
    let _ = app.update(Msg::Navigate(Page::Hosts));
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("1 host").is_ok());
    assert!(ui.find("Allocations").is_err());
    assert!(ui.find("Hardware").is_err());
    ui.click(iced::widget::Id::new("host-open")).unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(), [Msg::Navigate(Page::Host)]));
    let _ = app.update(messages[0].clone());
    assert_eq!(app.host_tab, HostTab::Overview);
    let mut ui = simulator(&app, (800.0, 600.0));
    for label in ["Overview", "Allocations", "Hardware"] {
        assert!(ui.find(label).is_ok());
    }
    ui.click("Back to hosts").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Navigate(Page::Hosts)]
    ));
}

#[test]
fn flat_allocations_keep_all_resources_and_expand_the_selected_persons_vms() {
    let _guard = lock();
    let mut app = host_fixture();
    app.host_tab = HostTab::Allocations;
    let mut ui = simulator(&app, (800.0, 600.0));
    for label in ["Memory", "CPU", "Storage"] {
        ui.click(label).unwrap();
    }
    assert!(ui.find("Alex's workspace").is_err());
    assert!(
        ui.into_messages().next().is_none(),
        "Resource headings must not act as a second navigation row"
    );
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Show VMs (1)").unwrap();
    let messages = ui.into_messages().collect::<Vec<_>>();
    assert!(matches!(messages.as_slice(),[Msg::HighlightOwner(Some(owner))] if *owner==id(2)));
    let _ = app.update(messages[0].clone());
    let mut ui = simulator(&app, (1180.0, 1040.0));
    ui.click("View VM").unwrap();
    assert!(
        matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),[Msg::OpenWorkspace(vm)] if *vm==id(12))
    );
}

#[test]
fn members_only_see_their_own_allocations_and_have_no_reserve_or_grant_controls() {
    let _guard = lock();
    let mut app = host_fixture();
    app.snapshot.as_mut().unwrap().me.role = Role::User;
    for page in [Page::Hosts, Page::Host] {
        app.page = page;
        for tab in [HostTab::Overview, HostTab::Allocations] {
            app.host_tab = tab;
            let mut ui = simulator(&app, (800.0, 600.0));
            for label in [
                "Alex Chen",
                "Alex's workspace",
                "Edit grant",
                "Host headroom",
                "Backed / VM capacity",
            ] {
                assert!(ui.find(label).is_err(), "Member sees {label}");
            }
        }
    }
}

#[test]
fn headroom_is_read_only_and_missing_policy_is_not_inferred_from_pool_free_space() {
    let _guard = lock();
    let mut app = host_fixture();
    let mut ui = simulator(&app, (1180.0, 1040.0));
    for label in [
        "Host headroom",
        "30 GiB",
        "Pool free-space floor",
        "Memory outside VM budget",
        "Cores outside VM budget",
    ] {
        assert!(ui.find(label).is_ok());
    }
    assert!(ui.find("Edit reserves").is_err());
    drop(ui);
    let s = app.snapshot.as_mut().unwrap();
    s.inventory.minimum_pool_available_bytes = None;
    s.inventory.pool_available_bytes = 999 * GIB;
    let mut ui = simulator(&app, (1180.0, 1040.0));
    assert!(ui.find("Not reported").is_ok());
    assert!(ui.find("30 GiB").is_err());
}
