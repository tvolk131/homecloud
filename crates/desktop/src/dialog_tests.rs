use super::*;
use crate::visual_tests::{fixture, id};
use iced::{Settings, Size};
use iced_test::Simulator;

pub(super) fn retained(app: &App, size: (f32, f32)) -> Simulator<'_, Msg, Theme> {
    Simulator::with_size(
        Settings {
            default_font: iced_m3::fonts::REGULAR,
            fonts: vec![iced_m3::fonts::ROBOTO.into()],
            ..Default::default()
        },
        Size::from(size),
        ui::retained_dialog(app),
    )
}
#[test]
fn dismissed_resource_forms_retain_their_native_geometry_including_errors() {
    for size in [(1180.0, 820.0), (800.0, 600.0)] {
        for panel in [
            Panel::EditWorkspace(id(10)),
            Panel::Create,
            Panel::Invite,
            Panel::Grant,
        ] {
            for invalid in [false, true] {
                let mut app = fixture();
                if matches!(panel, Panel::Grant) {
                    let _ = app.update(Msg::Select(id(2)));
                } else {
                    let _ = app.update(Msg::Open(panel.clone()));
                }
                if invalid && matches!(panel, Panel::EditWorkspace(_)) {
                    let _ = app.update(Msg::Edit(Field::Disk, "39".into()));
                }
                if invalid {
                    app.error = Some("Capacity changed; review the request.".into());
                }
                let geometry = |app: &App| {
                    let mut ui = retained(app, size);
                    [
                        "resource-slider-memory",
                        "resource-slider-cores",
                        "resource-slider-disk",
                        "resource-value-memory",
                        "resource-value-disk",
                    ]
                    .map(|name| {
                        ui.find(iced::widget::Id::from(name.to_owned()))
                            .unwrap()
                            .bounds()
                    })
                };
                let before = geometry(&app);
                let draft = app.fields.clone();
                let _ = app.update(Msg::Close);
                assert!(app.panel.is_none());
                assert!(resources::FormBounds::current(&app).is_none());
                assert_eq!(geometry(&app), before);
                assert_eq!(app.fields, draft);
                if invalid {
                    assert!(
                        retained(&app, size)
                            .find("Capacity changed; review the request.")
                            .is_ok()
                    );
                }
                // A duplicate dismissal must not swap the exit body for an empty dialog.
                let _ = app.update(Msg::Close);
                assert_eq!(geometry(&app), before);
                let _ = app.update(Msg::ResourceValue(Field::Ram, 1));
                assert_eq!(app.fields, draft);
            }
        }
    }
}

#[test]
fn closing_retains_display_bounds_but_new_forms_capture_fresh_capacity() {
    let mut app = fixture();
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
    let max = resources::FormBounds::displayed(&app).unwrap().memory.max;
    let _ = app.update(Msg::Close);
    app.snapshot.as_mut().unwrap().me.grant.memory_bytes = GIB;
    assert_eq!(
        resources::FormBounds::displayed(&app).unwrap().memory.max,
        max
    );
    assert!(resources::FormBounds::current(&app).is_none());
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
    assert_eq!(resources::FormBounds::current(&app).unwrap().memory.max, 4);
}

#[test]
fn capture_real_exit_frames_with_the_iced_runtime_cache() {
    use iced::advanced::renderer::Headless;
    use iced_test::{
        core::{clipboard, mouse, theme::Base, window},
        runtime::{UserInterface, user_interface},
    };
    use std::time::{Duration, Instant};
    let mut app = fixture();
    app.reduced_motion = false;
    let _ = app.update(Msg::Open(Panel::EditWorkspace(id(10))));
    // The simulator loads the same fonts used by production before raw frames.
    drop(retained(&app, (1180.0, 820.0)));
    let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        iced_m3::fonts::REGULAR,
        16.0.into(),
        Some("tiny-skia"),
    ))
    .unwrap();
    let mut cache = user_interface::Cache::default();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots");
    std::fs::create_dir_all(&root).unwrap();
    let frame = |app: &App, cache, renderer: &mut iced::Renderer, time: Instant, name: &str| {
        let mut interface =
            UserInterface::build(app.view(), Size::new(1180.0, 820.0), cache, renderer);
        let mut messages = Vec::new();
        interface.update(
            &[iced::Event::Window(window::Event::RedrawRequested(time))],
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let theme = app.theme();
        let base = theme.base();
        interface.draw(
            renderer,
            &theme,
            &iced::advanced::renderer::Style {
                text_color: base.text_color,
            },
            mouse::Cursor::Unavailable,
        );
        let rgba = renderer.screenshot(Size::new(2360, 1640), 2.0, base.background_color);
        image::save_buffer(
            root.join(format!("dialog-exit-{name}.png")),
            &rgba,
            2360,
            1640,
            image::ColorType::Rgba8,
        )
        .unwrap();
        assert!(messages.is_empty());
        interface.into_cache()
    };
    cache = frame(&app, cache, &mut renderer, Instant::now(), "open");
    let _ = app.update(Msg::Close);
    let start = Instant::now();
    for (millis, name) in [(0, "start"), (32, "32ms"), (64, "64ms")] {
        cache = frame(
            &app,
            cache,
            &mut renderer,
            start + Duration::from_millis(millis),
            name,
        );
    }
    assert_eq!(
        std::fs::read(root.join("dialog-exit-open.png")).unwrap(),
        std::fs::read(root.join("dialog-exit-start.png")).unwrap(),
        "Logical dismissal changed the image before the exit animation advanced"
    );
}
