use super::*;
use crate::visual_tests::{fixture, id, lock, scroll_bottom, simulator};

fn fab_id() -> iced::widget::Id {
    iced::widget::Id::new("page-fab")
}

#[test]
fn contextual_fabs_stay_in_the_viewport_while_content_scrolls() {
    let _guard = lock();
    for size in [(800.0, 600.0), (1180.0, 820.0), (2000.0, 900.0)] {
        for page in [Page::Workspaces, Page::Workspace(id(11))] {
            let mut app = fixture();
            app.page = page;
            app.advanced = true;
            let mut ui = simulator(&app, size);
            let before = ui.find(fab_id()).unwrap().bounds();
            assert_eq!(before.height, 56.0);
            assert!((before.x + before.width - (size.0 - 24.0)).abs() < 0.5);
            let viewport = ui
                .find(iced::widget::Id::new("page-body"))
                .unwrap()
                .bounds();
            if size.0 < 1500.0 {
                assert!((viewport.x + viewport.width - (size.0 - 8.0)).abs() < 0.5);
            }
            let scrollbar_left = viewport.x + viewport.width - 10.0;
            assert!(
                before.x + before.width <= scrollbar_left || before.x >= scrollbar_left + 10.0,
                "FAB overlaps the scrollbar: {before:?} vs {viewport:?}"
            );
            assert!((before.y + before.height - (size.1 - 24.0)).abs() < 0.5);
            assert!(before.width >= 120.0);
            scroll_bottom(&mut ui, size);
            assert_eq!(ui.find(fab_id()).unwrap().bounds(), before);
            assert_eq!(ui.find(fab_id()).unwrap().visible_bounds(), Some(before));
            let (label, other) = if matches!(page, Page::Workspaces) {
                ("New VM", "Edit VM")
            } else {
                ("Edit VM", "New VM")
            };
            assert!(ui.find(label).is_ok());
            assert!(ui.find(other).is_err());
        }
    }
}

#[test]
fn fabs_keep_navigation_click_through_and_route_the_selected_vm() {
    let _guard = lock();
    let mut app = fixture();
    let mut ui = simulator(&app, (1180.0, 820.0));
    assert!(ui.find("VMs").is_ok());
    assert!(ui.find("Machines").is_err());
    assert!(ui.find("Workspaces").is_err());
    ui.click("T3 development").unwrap();
    assert!(matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::OpenWorkspace(vm)] if *vm == id(10)));
    app.page = Page::Workspace(id(11));
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Edit VM").unwrap();
    assert!(matches!(ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Open(Panel::EditWorkspace(vm))] if *vm == id(11)));
    for page in [
        Page::Hosts,
        Page::Host,
        Page::People,
        Page::Activity,
        Page::Settings,
        Page::Workspace(id(999)),
    ] {
        app.page = page;
        assert!(simulator(&app, (800.0, 600.0)).find(fab_id()).is_err());
    }
    app.page = Page::Workspaces;
    app.snapshot.as_mut().unwrap().vms.clear();
    let mut ui = simulator(&app, (800.0, 600.0));
    assert!(ui.find("Create your first VM").is_err());
    ui.click("New VM").unwrap();
    assert!(matches!(
        ui.into_messages().collect::<Vec<_>>().as_slice(),
        [Msg::Open(Panel::Create)]
    ));
}

#[test]
fn unavailable_or_busy_fabs_and_modal_background_cannot_publish_actions() {
    let _guard = lock();
    for page in [Page::Workspaces, Page::Workspace(id(10))] {
        for offline in [false, true] {
            let mut app = fixture();
            app.page = page;
            app.busy = !offline;
            app.snapshot_unavailable = offline;
            let mut ui = simulator(&app, (1180.0, 820.0));
            ui.click(fab_id()).unwrap();
            assert!(ui.into_messages().next().is_none());
        }
        let mut app = fixture();
        app.page = page;
        let center = simulator(&app, (1180.0, 820.0))
            .find(fab_id())
            .unwrap()
            .bounds()
            .center();
        let panel = if matches!(page, Page::Workspaces) {
            Panel::Create
        } else {
            Panel::EditWorkspace(id(10))
        };
        let _ = app.update(Msg::Open(panel));
        let mut ui = simulator(&app, (1180.0, 820.0));
        ui.point_at(center);
        ui.simulate(iced_test::simulator::click());
        assert!(!ui.into_messages().any(|m| matches!(m, Msg::Open(_))));
    }
    let mut app = fixture();
    app.page = Page::Workspace(id(10));
    app.snapshot.as_mut().unwrap().operations[0].phase = "applying".into();
    let mut ui = simulator(&app, (1180.0, 820.0));
    ui.click(fab_id()).unwrap();
    assert!(ui.into_messages().next().is_none());
}

#[test]
fn contextual_fabs_are_reachable_by_keyboard() {
    use iced::keyboard::key::Named;
    let _guard = lock();
    for page in [Page::Workspaces, Page::Workspace(id(11))] {
        let mut app = fixture();
        app.page = page;
        assert!(
            (1..=24).any(|tabs| {
                let mut ui = simulator(&app, (800.0, 600.0));
                for _ in 0..tabs {
                    ui.tap_key(Named::Tab);
                }
                ui.tap_key(Named::Enter);
                ui.into_messages().any(|m| match page {
                    Page::Workspaces => matches!(m, Msg::Open(Panel::Create)),
                    _ => matches!(m, Msg::Open(Panel::EditWorkspace(vm)) if vm == id(11)),
                })
            }),
            "FAB missing from keyboard traversal on {page:?}"
        );
    }
}
