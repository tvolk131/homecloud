use super::{App, Field, Msg, Page, Panel, unix_now};
mod applications;
mod editor;
mod floating_actions;
mod hosts;
pub(crate) mod lifecycle;
mod memory_health;
mod reserves;
mod resource_controls;
mod setup;
mod shutdown;
mod status_colors;
mod vm_metrics;
mod workspace;
mod workspaces;
use homecloud_core::*;
use iced::{
    Alignment, Border, Color, Length,
    widget::{self, column, container, responsive, row, scrollable, text},
};
use iced_m3::{
    ButtonVariant as Variant, Element, Theme, TypeScale as Type, button,
    dialog::{dialog, modal},
    icon,
    material_symbols::{Fill, IconName, OpticalSize, Symbol},
    navigation::{NavigationItem, navigation_rail},
    typography,
};
use std::borrow::Cow;
fn horizontal_space() -> widget::Space {
    widget::space().width(Length::Fill)
}

fn copy<'a>(value: impl Into<Cow<'a, str>>) -> Element<'a, Msg> {
    typography(value.into(), Type::BodyMedium)
        .style(|theme: &Theme| text::Style {
            color: Some(theme.colors.on_surface_variant),
        })
        .into()
}
fn heading<'a>(value: impl widget::text::IntoFragment<'a>) -> Element<'a, Msg> {
    typography(value, Type::TitleLarge).into()
}
fn surface<'a>(
    content: impl Into<Element<'a, Msg>>,
    tonal: bool,
) -> widget::Container<'a, Msg, Theme> {
    container(content)
        .padding(24)
        .width(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            background: Some(
                if tonal {
                    theme.colors.primary_container
                } else {
                    theme.colors.surface_container
                }
                .into(),
            ),
            text_color: Some(if tonal {
                theme.colors.on_primary_container
            } else {
                theme.colors.on_surface
            }),
            border: Border {
                radius: 24.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
}
fn badge<'a>(label: impl Into<String>, good: bool) -> Element<'a, Msg> {
    let label = label.into();
    let attention = matches!(
        label.as_str(),
        "Needs review" | "Needs attention" | "Setup needed" | "Check host" | "Failed"
    );
    container(typography(label, Type::LabelMedium))
        .padding([5, 10])
        .style(move |theme: &Theme| container::Style {
            background: Some(
                if attention {
                    theme.colors.error_container
                } else if good {
                    status_colors::SUCCESS_CONTAINER
                } else {
                    theme.colors.surface_container_highest
                }
                .into(),
            ),
            text_color: Some(if attention {
                theme.colors.on_error_container
            } else if good {
                status_colors::ON_SUCCESS_CONTAINER
            } else {
                theme.colors.on_surface_variant
            }),
            border: Border {
                radius: 20.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}
fn secondary(label: &str, message: Msg) -> iced_m3::Button<'_, Msg> {
    button(label).variant(Variant::Outlined).on_press(message)
}
fn quiet(label: &str, message: Msg) -> iced_m3::Button<'_, Msg> {
    button(label).variant(Variant::Text).on_press(message)
}
fn danger(label: &str, message: Msg) -> iced_m3::Button<'_, Msg> {
    button(label)
        .palette(|theme| {
            (
                theme.colors.error_container,
                theme.colors.on_error_container,
            )
        })
        .on_press(message)
}
fn short(value: &str) -> String {
    if value.len() > 24 {
        format!("{}…{}", &value[..12], &value[value.len() - 8..])
    } else {
        value.into()
    }
}
fn gpu_name(value: &str) -> String {
    value
        .rsplit_once('[')
        .and_then(|(_, v)| v.split_once(']'))
        .map(|(v, _)| v.to_owned())
        .unwrap_or_else(|| value.to_owned())
}
// Content and floating actions share a 24px right edge. The scrollbar occupies
// the outer 16px gutter, rather than pushing the whole page farther inward.
const PAGE_EDGE_INSET: f32 = 24.0;
const SCROLLBAR_GUTTER: f32 = 16.0;

pub fn view(app: &App) -> Element<'_, Msg> {
    let background: Element<'_, Msg> = if let Some(snapshot) = &app.snapshot {
        responsive(move |size| {
            let items = [
                (Page::Workspaces, "VMs", IconName::DesktopWindows),
                (Page::Hosts, "Hosts", IconName::Dns),
                (Page::People, "People", IconName::Group),
                (Page::Activity, "Activity", IconName::History),
                (Page::Settings, "Settings", IconName::Settings),
            ]
            .into_iter()
            .filter(|(page, _, _)| *page != Page::People || snapshot.me.role == Role::Admin)
            .map(|(page, label, name)| {
                let symbol = Symbol::new(name);
                NavigationItem::new(page, label, symbol).selected_icon(symbol.fill(Fill::Filled))
            });
            let rail = navigation_rail(
                items,
                Some(if matches!(app.page, Page::Workspace(_)) {
                    Page::Workspaces
                } else if app.page == Page::Host {
                    Page::Hosts
                } else {
                    app.page
                }),
            )
            .on_select(Msg::Navigate)
            .header(container(icon(IconName::Cloud)).center_x(48).center_y(48))
            .footer(
                column![
                    badge(
                        snapshot.me.name.chars().next().unwrap_or('U').to_string(),
                        false
                    ),
                    copy(if snapshot.me.role == Role::Admin {
                        "Admin"
                    } else {
                        "Member"
                    })
                ]
                .align_x(Alignment::Center)
                .spacing(8),
            );
            let padding = if size.width < 950.0 { 20.0 } else { 32.0 };
            let right_padding = PAGE_EDGE_INSET - SCROLLBAR_GUTTER;
            let width = (size.width - 96.0 - padding - right_padding).max(400.0);
            let title = match app.page {
                Page::Workspaces => "VMs",
                Page::Workspace(id) => snapshot
                    .vms
                    .iter()
                    .find(|v| v.id == id)
                    .map(|v| v.spec.name.as_str())
                    .unwrap_or("VM"),
                Page::Hosts => "Hosts",
                Page::Host => hosts::hostname(snapshot),
                Page::People => "People",
                Page::Activity => "Activity",
                Page::Settings => "Settings",
            };
            let subtitle = match app.page {
                Page::Workspaces => "Your development machines, in one place.",
                Page::Workspace(_) => "VM details",
                Page::Hosts => "Physical machines and their VM capacity.",
                Page::Host => "Capacity, people, and hardware on this machine.",
                Page::People => "People and devices with access to your cloud.",
                Page::Activity => "Requests, progress, and anything that needs attention.",
                Page::Settings => "Preferences for this Homecloud client.",
            };
            let primary: Element<'_, Msg> = match app.page {
                Page::Workspace(_) => quiet("Back to VMs", Msg::Navigate(Page::Workspaces)).into(),
                Page::Host => quiet("Back to hosts", Msg::Navigate(Page::Hosts)).into(),
                Page::Workspaces | Page::Settings => widget::space().into(),
                Page::People if snapshot.me.role == Role::Admin => button("+  Invite coworker")
                    .on_press(Msg::Open(Panel::Invite))
                    .disabled(app.busy)
                    .into(),
                _ => quiet(
                    if app.refreshing {
                        "Refreshing…"
                    } else {
                        "Refresh"
                    },
                    Msg::Refresh,
                )
                .disabled(app.refreshing)
                .into(),
            };
            let default_header: Element<'_, Msg> = row![
                column![typography(title, Type::HeadlineLarge), copy(subtitle)]
                    .spacing(4)
                    .width(Length::Fill),
                primary
            ]
            .spacing(16)
            .align_y(Alignment::Center)
            .into();
            let header = if let Page::Workspace(id) = app.page {
                workspace::header(app, snapshot, id, width)
            } else if app.page == Page::Host {
                column![default_header, hosts::navigation(app)]
                    .spacing(16)
                    .into()
            } else {
                default_header
            };
            let body = match app.page {
                Page::Workspaces => workspaces::view(app, snapshot, width),
                Page::Workspace(id) => workspace::view(app, snapshot, id, width),
                Page::Hosts => hosts::directory(app, snapshot),
                Page::Host => hosts::view(app, snapshot, width - 16.0),
                Page::People if snapshot.me.role == Role::Admin => people(app, snapshot),
                Page::People => workspaces::view(app, snapshot, width),
                Page::Activity => activity(app, snapshot),
                Page::Settings => settings(app),
            };
            let fab = floating_actions::action(app, snapshot);
            let bottom_clearance = if fab.is_some() {
                floating_actions::clearance(padding)
            } else {
                16.0
            };
            let mut main = column![container(header).padding(iced::Padding {
                right: SCROLLBAR_GUTTER,
                ..Default::default()
            })]
            .spacing(24);
            if app.panel.is_none() {
                if let Some(e) = &app.error {
                    main = main.push(container(message(e, true)).padding(iced::Padding {
                        right: SCROLLBAR_GUTTER,
                        ..Default::default()
                    }));
                } else if !app.notice.is_empty() {
                    main = main.push(container(message(&app.notice, false)).padding(
                        iced::Padding {
                            right: SCROLLBAR_GUTTER,
                            ..Default::default()
                        },
                    ));
                }
            }
            if let Some(warning) = memory_health::banner(app, snapshot) {
                main = main.push(container(warning).padding(iced::Padding {
                    right: SCROLLBAR_GUTTER,
                    ..Default::default()
                }));
            }
            if let Some(status) = shutdown::banner(app, snapshot) {
                main = main.push(status);
            }
            main = main.push(
                scrollable(
                    container(body)
                        .padding(iced::Padding {
                            right: SCROLLBAR_GUTTER,
                            bottom: bottom_clearance,
                            ..Default::default()
                        })
                        .width(Length::Fill),
                )
                .id("page-body")
                .width(Length::Fill)
                .height(Length::Fill),
            );
            let content = container(main)
                .padding(iced::Padding {
                    top: padding,
                    right: right_padding,
                    bottom: padding,
                    left: padding,
                })
                .max_width(1350)
                .height(Length::Fill);
            let pane: Element<'_, Msg> = container(content)
                .center_x(Length::Fill)
                .height(Length::Fill)
                .into();
            let pane: Element<'_, Msg> = if let Some(fab) = fab {
                widget::stack![pane, floating_actions::positioned(fab)].into()
            } else {
                pane
            };
            row![rail, pane].height(Length::Fill).into()
        })
        .into()
    } else {
        pairing(app)
    };
    let panel = displayed_dialog(app);
    iced_m3::focus::scope(modal(background, panel, app.panel.is_some()))
}
fn message(value: &str, error: bool) -> Element<'_, Msg> {
    container(
        row![
            typography(value, Type::BodyMedium).width(Length::Fill),
            quiet("Dismiss", Msg::DismissNotice)
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([8, 16])
    .width(Length::Fill)
    .style(move |theme: &Theme| container::Style {
        background: Some(
            if error {
                theme.colors.error_container
            } else {
                theme.colors.secondary_container
            }
            .into(),
        ),
        text_color: Some(if error {
            theme.colors.on_error_container
        } else {
            theme.colors.on_secondary_container
        }),
        border: Border {
            radius: 16.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}
fn vm_stat(value: String, label: &'static str) -> Element<'static, Msg> {
    column![typography(value, Type::TitleMedium), copy(label)]
        .spacing(4)
        .width(Length::Fill)
        .into()
}
fn settings(app: &App) -> Element<'_, Msg> {
    surface(
        row![
            column![
                heading("Motion"),
                copy("Use fewer transitions and shape animations.")
            ]
            .spacing(6)
            .width(Length::Fill),
            iced_m3::switch(app.reduced_motion)
                .label("Reduce motion")
                .on_toggle(|_| Msg::ToggleMotion)
        ]
        .spacing(16)
        .align_y(Alignment::Center),
        false,
    )
    .into()
}
fn people<'a>(app: &'a App, s: &'a Snapshot) -> Element<'a, Msg> {
    let mut body = column![heading("Resource grants")].spacing(16);
    for user in &s.users {
        body = body.push(surface(
            row![
                column![
                    typography(&user.name, Type::TitleMedium),
                    copy(format!(
                        "{} · {} GiB RAM · {} cores · {} GiB disk",
                        if user.role == Role::Admin {
                            "Administrator"
                        } else {
                            "Member"
                        },
                        user.grant.memory_bytes / GIB,
                        user.grant.cpu_cores,
                        user.grant.disk_bytes / GIB
                    ))
                ]
                .spacing(6)
                .width(Length::Fill),
                badge(
                    if !user.active {
                        "Disabled"
                    } else if user.gpu_allowed {
                        "GPU allowed"
                    } else {
                        "Active"
                    },
                    user.active
                ),
                secondary("Edit grant", Msg::Select(user.id)).disabled(app.busy)
            ]
            .spacing(16)
            .align_y(Alignment::Center),
            false,
        ));
    }
    body = body.push(heading("Authorized devices")).push(copy(
        "Revoking a device closes its active tunnels and prevents it from reconnecting.",
    ));
    for device in &s.devices {
        let owner = s
            .users
            .iter()
            .find(|u| u.id == device.user_id)
            .map(|u| u.name.as_str())
            .unwrap_or("User");
        body = body.push(surface(
            row![
                column![
                    typography(format!("{owner}'s device"), Type::TitleMedium),
                    copy(short(&device.peer))
                ]
                .spacing(6)
                .width(Length::Fill),
                badge(
                    if device.active {
                        "Authorized"
                    } else {
                        "Revoked"
                    },
                    device.active
                ),
                quiet("Revoke", Msg::Open(Panel::Revoke(device.peer.clone())))
                    .disabled(app.busy || !device.active)
            ]
            .spacing(16)
            .align_y(Alignment::Center),
            false,
        ));
    }
    body.into()
}
fn operation_name(op: &Operation) -> &'static str {
    match &op.request {
        Mutation::Create { .. } => "Create VM",
        Mutation::Start {
            without_gpu: true, ..
        } => "Start without GPUs",
        Mutation::Start { .. } => "Start VM",
        Mutation::ForceStop { .. } => "Force stop VM",
        Mutation::State {
            desired: Lifecycle::Running,
            ..
        } => "Start VM",
        Mutation::State {
            desired: Lifecycle::Stopped,
            ..
        } => "Stop VM",
        Mutation::State { .. } => "Delete VM",
        Mutation::Resize { .. } => "Resize VM",
        Mutation::Edit { .. } => "Edit VM",
        Mutation::Gpu { pci: Some(_), .. } => "Attach GPU",
        Mutation::Gpu { .. } => "Detach GPU",
    }
}
fn activity<'a>(app: &'a App, s: &'a Snapshot) -> Element<'a, Msg> {
    let mut body = column![].spacing(12);
    if s.operations.is_empty() {
        body = body.push(surface(
            column![
                heading("Everything is quiet"),
                copy("VM requests and resource changes will appear here.")
            ]
            .spacing(12),
            false,
        ));
    }
    for op in &s.operations {
        let name = s
            .vms
            .iter()
            .find(|v| v.id == op.vm_id)
            .map(|v| v.spec.name.clone())
            .unwrap_or_else(|| format!("VM {}", &op.vm_id.to_string()[..8]));
        let phase = match op.phase.as_str() {
            "succeeded" => "Completed",
            "needs_review" => "Needs review",
            "pending" => "Queued",
            "applying" | "retrying" => "In progress",
            "failed" => "Failed",
            _ => "Status unknown",
        };
        let mut details = column![
            typography(operation_name(op), Type::TitleMedium),
            copy(name)
        ]
        .spacing(6)
        .width(Length::Fill);
        if let Some(error) = &op.error {
            details = details.push(copy(error.strip_prefix("pending: ").unwrap_or(error)));
        }
        let mut line = row![details, badge(phase, op.phase == "succeeded")]
            .spacing(16)
            .align_y(Alignment::Center);
        if op.phase == "needs_review" && s.me.role == Role::Admin {
            line = line.push(secondary("Retry after review", Msg::Retry(op.id)).disabled(app.busy));
        }
        body = body.push(surface(line, false));
    }
    body.into()
}
fn pairing(app: &App) -> Element<'_, Msg> {
    let connecting = app.client.is_some();
    let mut content = column![
        container(icon(IconName::Cloud)).padding(12),
        typography(
            if connecting {
                "Connecting to your cloud"
            } else {
                "Your cloud. Your VMs."
            },
            Type::HeadlineLarge
        ),
        copy(if connecting {
            "Fetching your machines and host status…"
        } else {
            "Pair this Mac once, then manage your development machines from anywhere."
        })
    ]
    .spacing(20);
    if !connecting {
        content=content.push(heading("Pair this Mac")).push(copy("Verify the host identity using your trusted connection, then enter its public ticket and invitation code."))
            .push(app.input("Public host ticket",Field::Ticket))
            .push(iced_m3::text_field("Invitation code",app.get(Field::Token)).secure(true).on_input(|s|Msg::Edit(Field::Token,s)))
            .push(button(if app.busy{"Pairing…"}else{"Pair device"}).on_press(Msg::Pair).disabled(app.busy||app.get(Field::Token).is_empty()||app.get(Field::Ticket).is_empty()));
    }
    if let Some(error) = &app.error {
        content = content.push(message(error, true));
    }
    container(surface(content, false).max_width(540))
        .padding(32)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}
fn displayed_dialog(app: &App) -> iced_m3::Dialog<'_, Msg> {
    app.panel
        .as_ref()
        .or(app.last_panel.as_ref())
        .map(|panel| panel_view(app, panel))
        .unwrap_or_else(|| dialog(text("")).on_dismiss(Msg::Close))
}
fn panel_error(app: &App) -> Option<&String> {
    if app.panel.is_some() {
        app.error.as_ref()
    } else {
        app.last_panel_error.as_ref()
    }
}
#[cfg(test)]
pub(super) fn retained_dialog(app: &App) -> Element<'_, Msg> {
    // Hold the actual exit body visible so tests can inspect its layout after
    // logical dismissal; the production modal owns animation and input gating.
    iced_m3::focus::scope(modal(
        widget::space().width(Length::Fill).height(Length::Fill),
        displayed_dialog(app),
        true,
    ))
}
fn panel_view<'a>(app: &'a App, panel: &'a Panel) -> iced_m3::Dialog<'a, Msg> {
    let mut content = column![].spacing(20);
    let mut actions = row![quiet("Cancel", Msg::Close).disabled(app.busy)].spacing(8);
    match panel {
        Panel::ShutdownHost {
            boot_id, hostname, ..
        } => {
            let count = app
                .snapshot
                .as_ref()
                .map(|s| {
                    s.vms
                        .iter()
                        .filter(|v| v.observed == "running" || v.desired == Lifecycle::Running)
                        .count()
                })
                .unwrap_or(0);
            content=content.push(heading(format!("Shut down “{hostname}”?")))
                .push(copy(format!("{count} running VM{} will shut down. Everyone’s running tasks will be interrupted, and terminal and T3 connections will disconnect.",if count==1 {""} else {"s"})))
                .push(copy("VMs currently set to run will start again when the host is powered back on. Their disks and allocations are kept."))
                .push(copy("You’ll need physical access or separately configured remote power management to turn this host back on."));
            let changed = app
                .snapshot
                .as_ref()
                .is_none_or(|s| s.inventory.shutdown_boot_id.as_ref() != Some(boot_id));
            if changed {
                content = content.push(message(
                    "Host restarted. Close this dialog and review its current status.",
                    true,
                ));
            }
            actions = actions.push(
                danger(
                    if app.busy {
                        "Requesting shutdown…"
                    } else {
                        "Shut down host"
                    },
                    Msg::ShutdownHost,
                )
                .disabled(!app.can_shutdown_host() || changed),
            );
        }
        Panel::SetupDiagnostics(id) => {
            return dialog(setup::details(app, *id))
                .actions(
                    row![
                        quiet("Refresh", Msg::LoadSetupDiagnostics(*id))
                            .disabled(app.setup_diagnostics_loading),
                        quiet("Done", Msg::Close)
                    ]
                    .spacing(8),
                )
                .width(720.0)
                .max_height(680.0)
                .on_dismiss(Msg::Close);
        }
        Panel::ReviewMemoryCapacity => {
            if let Some(s) = &app.snapshot
                && let Some(m) = &app.memory_review
            {
                let changed = s
                    .host_reserves
                    .as_ref()
                    .and_then(|v| v.memory.as_ref())
                    .is_none_or(|live| {
                        live.revision != m.revision || live.safe_bytes != m.safe_bytes
                    });
                let promised = s
                    .users
                    .iter()
                    .map(|u| u.grant.memory_bytes.max(u.committed.memory_bytes) as u128)
                    .sum::<u128>();
                let held = s
                    .users
                    .iter()
                    .map(|u| u.committed.memory_bytes as u128)
                    .sum::<u128>();
                content = content.push(heading("Review memory capacity"))
                        .push(copy(format!("Usable physical RAM: {:.1} GiB. System minimum: {} GiB.", m.physical_bytes as f64 / GIB as f64, m.minimum_system_bytes/GIB)))
                        .push(copy(format!("VM envelope: {} → {} GiB.",m.reviewed_bytes/GIB,m.safe_bytes/GIB)))
                        .push(copy(format!("User promises: {} GiB. Held VM memory: {} GiB.",promised/GIB as u128,held/GIB as u128)))
                        .push(copy("All grants and VM claims are kept. If promises exceed physical capacity, additions stay blocked until an administrator reduces grants. VM starts wait while held memory exceeds capacity."));
                if changed {
                    content =
                        content.push(copy("Memory capacity changed. Close and review it again."));
                }
                actions = actions.push(
                    button("Adopt capacity")
                        .on_press(Msg::AdoptMemoryCapacity)
                        .disabled(app.busy || !m.arc_ready || app.snapshot_unavailable || changed),
                );
            }
        }
        Panel::HostReserves => {
            let (body, buttons) = reserves::form(app);
            return dialog(body)
                .actions(buttons)
                .width(600.0)
                .max_height(720.0)
                .on_dismiss(Msg::Close)
                .dismiss_on_outside(!app.busy)
                .dismiss_on_escape(!app.busy);
        }
        Panel::EditWorkspace(id) => {
            let (body, buttons) = editor::form(app, *id);
            content = content.push(body);
            return dialog(content)
                .actions(buttons)
                .width(600.0)
                .max_height(720.0)
                .on_dismiss(Msg::Close)
                .dismiss_on_outside(!app.busy)
                .dismiss_on_escape(!app.busy);
        }
        Panel::ConfirmEdit { vm_id, changes } => {
            let (body, buttons) = editor::confirmation(app, *vm_id, changes);
            content = content.push(body);
            return dialog(content)
                .actions(buttons)
                .width(560.0)
                .max_height(700.0)
                .on_dismiss(Msg::Close)
                .dismiss_on_outside(!app.busy)
                .dismiss_on_escape(!app.busy);
        }
        Panel::Create => {
            if let Some(pending) = app.displayed_create() {
                let spec = &pending.spec;
                content = content.push(heading("Confirm VM creation"))
                    .push(typography(spec.name.clone(), Type::TitleMedium))
                    .push(copy(format!("{} GiB RAM · {} {} · {} GiB disk", spec.memory_gib, spec.cores, if spec.cores == 1 { "core" } else { "cores" }, spec.disk_gib)))
                    .push(copy(if spec.managed_host() { "Homecloud host with 32 GiB test disk" } else if spec.applications.contains(&ApplicationKind::T3) { "T3 Code included" } else { "Ubuntu VM" }))
                    .push(copy("Your request is saved. Retry checks the original submission without creating another VM."))
                    .push(row![
                        iced_m3::circular_progress(0.0).indeterminate(app.busy).paused(app.reduced_motion).size(20.0),
                        copy(if app.busy { "Checking the request…" } else { "Waiting for confirmation" }),
                    ].spacing(12).align_y(Alignment::Center))
                    .push(copy("Closing this dialog does not cancel VM creation."));
                actions = row![
                    quiet("Close", Msg::Close).disabled(app.busy),
                    button(if app.busy {
                        "Checking…"
                    } else {
                        "Retry request"
                    })
                    .on_press(Msg::Create)
                    .disabled(app.busy || app.create_error.is_some()),
                ]
                .spacing(8);
                return dialog(content)
                    .actions(actions)
                    .width(560.0)
                    .max_height(680.0)
                    .on_dismiss(Msg::Close)
                    .dismiss_on_outside(!app.busy)
                    .dismiss_on_escape(!app.busy);
            }
            if let Some(error) = &app.create_error {
                content = content.push(copy(error.clone()));
            }
            let selected = if app.install_managed_host {
                Some(ApplicationKind::HomecloudHost)
            } else if app.install_t3 {
                Some(ApplicationKind::T3)
            } else {
                None
            };
            content = content.push(heading("Create a VM"));
            let mut workloads = column![
                iced_m3::radio("Ubuntu VM", None, Some(selected)).on_select(Msg::SelectWorkload),
                iced_m3::radio("T3 Code", Some(ApplicationKind::T3), Some(selected))
                    .on_select(Msg::SelectWorkload),
            ]
            .spacing(4);
            if app
                .snapshot
                .as_ref()
                .is_some_and(|s| s.me.role == Role::Admin)
            {
                workloads = workloads.push(
                    iced_m3::radio(
                        "Homecloud host lab",
                        Some(ApplicationKind::HomecloudHost),
                        Some(selected),
                    )
                    .on_select(Msg::SelectWorkload),
                );
            }
            content=content.push(workloads).push(copy(if app.install_managed_host {
                "A separate controller, Incus and ZFS pool. Includes a backed 32 GiB test disk. Nested KVM and a prepared host bundle are required."
            } else if app.install_t3 {
                "Includes Codex and Claude Code. 4 GiB RAM recommended; sign in after installation."
            } else { "An isolated Ubuntu VM with persistent storage." }))
                .push(app.input("Machine name",Field::Name))
                .push(resource_controls::controls(app))
                .push(resource_controls::validation(app,format!("Whole cores include SMT siblings. Disk backing includes {} GiB overhead{}.",app.snapshot.as_ref().map(|s|s.disk_overhead_gib_per_vm).unwrap_or(2),if app.install_managed_host { " plus the 32 GiB test disk" } else { " per VM" })))
                .push(app.input("Guest SSH public key",Field::Key))
                .push(copy("Only a public key. Your private key stays on this Mac.")).spacing(12);
            actions = actions.push(
                button(if app.busy {
                    "Submitting…"
                } else {
                    "Create VM"
                })
                .on_press(Msg::Create)
                .disabled(
                    app.busy
                        || app.create_error.is_some()
                        || app.snapshot_unavailable
                        || crate::resources::FormBounds::current(app)
                            .is_none_or(|b| b.validate(app).is_err()),
                ),
            );
        }
        Panel::GpuUnavailable(conflict) => {
            let name = app
                .snapshot
                .as_ref()
                .and_then(|s| s.vms.iter().find(|v| v.id == conflict.vm_id))
                .map(|v| v.spec.name.as_str())
                .unwrap_or("This VM");
            let model = app
                .snapshot
                .as_ref()
                .and_then(|s| s.inventory.gpus.iter().find(|g| g.pci == conflict.pci))
                .map(|g| gpu_name(&g.model))
                .unwrap_or_else(|| "Remembered GPU".into());
            content = content.push(heading("GPU unavailable"))
                .push(typography(format!("{name} is still stopped"), Type::TitleMedium))
                .push(copy(format!("{model} · {}", conflict.pci)))
                .push(copy(&conflict.reason))
                .push(copy("Go back to try again later, or start with none of the selected GPUs. All GPU preferences will be remembered for the next start."));
            actions = row![
                quiet("Go back", Msg::Close).disabled(app.busy),
                button(if app.busy {
                    "Submitting…"
                } else {
                    "Start without GPUs"
                })
                .on_press(Msg::StartWithoutGpu(conflict.vm_id))
                .disabled(app.busy)
            ]
            .spacing(8);
        }
        Panel::Invite | Panel::Grant => {
            let invite = matches!(panel, Panel::Invite);
            content = content.push(heading(if invite {
                "Invite a coworker"
            } else {
                "Edit resource grant"
            }));
            if invite {
                content = content.push(app.input("Display name", Field::User));
            } else {
                content = content.push(copy(app.get(Field::User)));
            }
            content=content.push(resource_controls::controls(app))
                .push(resource_controls::validation(app,"Grants reserve capacity on this host. Availability is checked again when saving.".into()))
                .push(iced_m3::switch(app.gpu).label("Allow GPU assignment").on_toggle(|_|Msg::ToggleGpu))
                .push(copy(if invite{"The invitation expires after 24 hours and can enroll one device."}else{"Reducing a grant blocks further growth. Existing VMs and GPU assignments keep running."})).spacing(12);
            actions = actions.push(
                button(if invite {
                    "Create invitation"
                } else {
                    "Save grant"
                })
                .on_press(if invite { Msg::Invite } else { Msg::Grant })
                .disabled(
                    app.busy
                        || app.snapshot_unavailable
                        || (invite && app.get(Field::User).trim().is_empty())
                        || crate::resources::FormBounds::current(app)
                            .is_none_or(|b| b.validate(app).is_err()),
                ),
            );
        }
        Panel::Invitation => {
            content=content.push(heading("Invitation ready")).push(copy("Share this single-use code privately, together with the verified public host ticket. It expires in 24 hours.")).push(copy("The code is kept hidden here so it stays out of screenshots."));
            actions = row![
                quiet("Done", Msg::Close),
                button("Copy invitation code").on_press(Msg::CopyInvite)
            ];
        }
        Panel::Delete(id) => {
            let name = app
                .snapshot
                .as_ref()
                .and_then(|s| s.vms.iter().find(|v| v.id == *id))
                .map(|v| v.spec.name.as_str())
                .unwrap_or("this machine");
            content=content.push(heading(format!("Delete {name}?"))).push(copy("This permanently deletes the VM and its disk. Its resource commitments are released after the host confirms deletion."));
            actions =
                actions.push(danger("Delete permanently", Msg::Delete(*id)).disabled(app.busy));
        }
        Panel::ForceStop(id) => {
            let name = app
                .snapshot
                .as_ref()
                .and_then(|s| s.vms.iter().find(|v| v.id == *id))
                .map(|v| v.spec.name.as_str())
                .unwrap_or("this machine");
            content = content.push(heading(format!("Force stop {name}?")))
                .push(copy("This powers off the VM without waiting for its operating system to shut down. Unsaved work and pending disk writes may be lost."))
                .push(copy("Its disk is retained. CPU, RAM and GPU allocations stay held until the host confirms shutdown and cleanup."));
            actions = actions.push(
                danger("Force stop", Msg::ForceStop(*id))
                    .disabled(app.busy || app.snapshot_unavailable),
            );
        }
        Panel::Stop(id) => {
            let name = app
                .snapshot
                .as_ref()
                .and_then(|s| s.vms.iter().find(|v| v.id == *id))
                .map(|v| v.spec.name.as_str())
                .unwrap_or("this machine");
            content = content.push(heading(format!("Stop {name}?")))
                .push(copy("This shuts down the VM and ends its running terminals and tasks. Its disk and installed applications are kept."))
                .push(copy("CPU, RAM and the GPU become available after shutdown and cleanup finish."));
            actions = actions.push(
                button("Stop VM")
                    .on_press(Msg::Stop(*id))
                    .disabled(app.busy || app.snapshot_unavailable),
            );
        }
        Panel::RemoveGitHub(id) => {
            content = content
                .push(heading("Remove GitHub access from this VM?"))
                .push(copy("Git and gh in this VM, including tasks running in T3, will no longer be able to use your GitHub account. Repositories and commits in the VM are kept."))
                .push(copy("This doesn’t revoke the token on GitHub. To revoke it, open GitHub Settings → Applications → GitHub CLI. Doing that also signs out gh on your other devices."));
            actions = actions.push(
                danger(
                    "Remove access",
                    Msg::SetupApplication(*id, ApplicationSetupAction::SignOutGitHub),
                )
                .disabled(app.busy || app.snapshot_unavailable || app.app_request.is_some()),
            );
        }
        Panel::Revoke(peer) => {
            content=content.push(heading("Revoke this device?")).push(copy(short(peer))).push(copy("Its active SSH tunnels will close, and it will no longer be able to connect. Enrolling it again requires a fresh device key."));
            actions =
                actions.push(danger("Revoke access", Msg::Revoke(peer.clone())).disabled(app.busy));
        }
    }
    if let Some(error) = panel_error(app) {
        content = content.push(message(error, true));
    }
    dialog(content)
        .actions(actions.wrap())
        .width(540.0)
        .max_height(700.0)
        .on_dismiss(Msg::Close)
        .dismiss_on_outside(!app.busy)
        .dismiss_on_escape(!app.busy)
}
