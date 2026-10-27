use super::{App, Field, Msg, Page, Panel};
use homecloud_core::*;
use iced::{
    Alignment, Border, Color, Length,
    widget::{self, column, container, responsive, row, scrollable, text},
};
use iced_m3::{
    ButtonVariant as Variant, Element, Theme, TypeScale as Type, button,
    dialog::{dialog, modal},
    icon,
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
fn glyph<'a>(name: &str) -> Element<'a, Msg> {
    let path = match name {
        "cloud" => {
            "M7 18a5 5 0 0 1-1-9.9 6 6 0 0 1 11.5-1.2A5.5 5.5 0 0 1 18 18H7Zm3-8-3 3 3 3 1.4-1.4L9.8 13l1.6-1.6L10 10Zm4 6 3-3-3-3-1.4 1.4 1.6 1.6-1.6 1.6L14 16Z"
        }
        "vm" => {
            "M4 3h16a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-7v2h4v2H7v-2h4v-2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Zm0 2v12h16V5H4Zm3 2 4 4-4 4-1.4-1.4L8.2 11 5.6 8.4 7 7Zm5 6h6v2h-6v-2Z"
        }
        "host" => {
            "M7 2h10v3h3v2h3v2h-3v6h3v2h-3v2h-3v3H7v-3H4v-2H1v-2h3V9H1V7h3V5h3V2Zm-1 5v10h12V7H6Zm2 2h8v6H8V9Z"
        }
        "people" => {
            "M9 3a4 4 0 1 1 0 8 4 4 0 0 1 0-8Zm0 2a2 2 0 1 0 0 4 2 2 0 0 0 0-4Zm8-1a3.5 3.5 0 0 1 0 7V9a1.5 1.5 0 0 0 0-3V4ZM1 20v-3c0-3 5-5 8-5s8 2 8 5v3H1Zm2-2h12v-1c0-1.4-3.6-3-6-3s-6 1.6-6 3v1Zm15-5c3 .5 5 2 5 4v3h-4v-2h2v-1c0-.9-1.3-1.7-3-2v-2Z"
        }
        "activity" => {
            "M12 2a10 10 0 1 1-9.5 13h2.1A8 8 0 1 0 5.4 7.5L8 10H1V3l3 3A10 10 0 0 1 12 2Zm-1 4h2v6.4l4 2.4-1 1.7-5-3V6Z"
        }
        _ => "M4 4h16v16H4V4Zm2 2v12h12V6H6Z",
    };
    icon(widget::svg::Handle::from_memory(format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><path d=\"{path}\"/></svg>").into_bytes())).into()
}
fn badge<'a>(label: impl Into<String>, good: bool) -> Element<'a, Msg> {
    let label = label.into();
    let attention = matches!(
        label.as_str(),
        "Needs review" | "Needs attention" | "Setup needed" | "Check host"
    );
    container(typography(label, Type::LabelMedium))
        .padding([5, 10])
        .style(move |theme: &Theme| container::Style {
            background: Some(
                if attention {
                    theme.colors.error_container
                } else if good {
                    Color::from_rgb8(24, 58, 46)
                } else {
                    theme.colors.surface_container_highest
                }
                .into(),
            ),
            text_color: Some(if attention {
                theme.colors.on_error_container
            } else if good {
                Color::from_rgb8(154, 224, 183)
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
fn state_label(vm: &Vm) -> &str {
    if vm
        .error
        .as_ref()
        .is_some_and(|e| !e.starts_with("pending:"))
    {
        "Needs attention"
    } else if vm.error.is_some() {
        "Updating"
    } else {
        match vm.observed.as_str() {
            "running" => "Running",
            "stopped" => "Stopped",
            "pending" => "Creating",
            _ => "Check status",
        }
    }
}

pub fn view(app: &App) -> Element<'_, Msg> {
    let background: Element<'_, Msg> = if let Some(snapshot) = &app.snapshot {
        responsive(move |size| {
            let items = [
                (Page::Workspaces, "Workspaces", "vm"),
                (Page::Host, "Host", "host"),
                (Page::People, "People", "people"),
                (Page::Activity, "Activity", "activity"),
            ]
            .into_iter()
            .filter(|(page, _, _)| *page != Page::People || snapshot.me.role == Role::Admin)
            .map(|(page, label, icon)| NavigationItem::new(page, label, glyph(icon)));
            let rail = navigation_rail(items, Some(app.page))
                .on_select(Msg::Navigate)
                .header(container(glyph("cloud")).center_x(48).center_y(48))
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
            let width = (size.width - 96.0 - padding * 2.0).max(400.0);
            let title = match app.page {
                Page::Workspaces => "Workspaces",
                Page::Host => "Host",
                Page::People => "People",
                Page::Activity => "Activity",
            };
            let subtitle = match app.page {
                Page::Workspaces => "Your development machines, in one place.",
                Page::Host => "Capacity, storage, and hardware readiness.",
                Page::People => "People and devices with access to your cloud.",
                Page::Activity => "Requests, progress, and anything that needs attention.",
            };
            let primary: Element<'_, Msg> = match app.page {
                Page::Workspaces => button("+  New VM")
                    .on_press(Msg::Open(Panel::Create))
                    .disabled(app.busy)
                    .into(),
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
            let header = row![
                column![typography(title, Type::HeadlineLarge), copy(subtitle)]
                    .spacing(4)
                    .width(Length::Fill),
                primary
            ]
            .spacing(16)
            .align_y(Alignment::Center);
            let body = match app.page {
                Page::Workspaces => workspaces(app, snapshot, width),
                Page::Host => host(app, snapshot),
                Page::People if snapshot.me.role == Role::Admin => people(app, snapshot),
                Page::People => workspaces(app, snapshot, width),
                Page::Activity => activity(app, snapshot),
            };
            let mut main = column![header].spacing(24);
            if app.panel.is_none() {
                if let Some(e) = &app.error {
                    main = main.push(message(e, true));
                } else if !app.notice.is_empty() {
                    main = main.push(message(&app.notice, false));
                }
            }
            main = main.push(
                scrollable(
                    container(body)
                        .padding(iced::Padding {
                            right: 16.0,
                            bottom: 16.0,
                            ..Default::default()
                        })
                        .width(Length::Fill),
                )
                .width(Length::Fill)
                .height(Length::Fill),
            );
            let content = container(main)
                .padding(padding)
                .max_width(1350)
                .height(Length::Fill);
            row![
                rail,
                container(content)
                    .center_x(Length::Fill)
                    .height(Length::Fill)
            ]
            .height(Length::Fill)
            .into()
        })
        .into()
    } else {
        pairing(app)
    };
    let panel = app
        .panel
        .as_ref()
        .or(app.last_panel.as_ref())
        .map(|panel| panel_view(app, panel))
        .unwrap_or_else(|| dialog(text("")).on_dismiss(Msg::Close));
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
fn metric(label: &str, used: u64, grant: u64, unit: &str) -> Element<'static, Msg> {
    let over = used > grant;
    surface(
        column![
            copy(label.to_owned()),
            row![
                typography(used.to_string(), Type::HeadlineLarge),
                copy(format!("/ {grant} {unit}"))
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            iced_m3::linear_progress(if grant == 0 {
                if used == 0 { 0.0 } else { 1.0 }
            } else {
                (used as f32 / grant as f32).min(1.0)
            }),
            copy(if over {
                format!("{} {unit} over grant · growth paused", used - grant)
            } else {
                format!("{} {unit} available", grant - used)
            })
        ]
        .spacing(12),
        false,
    )
    .into()
}
fn compact_metric(label: &str, used: u64, grant: u64, unit: &str) -> Element<'static, Msg> {
    column![
        copy(label.to_owned()),
        typography(format!("{used} / {grant}"), Type::TitleLarge),
        copy(if used > grant {
            format!("{unit} · over grant")
        } else {
            format!("{unit} allocated")
        })
    ]
    .spacing(6)
    .width(Length::Fill)
    .into()
}
fn workspaces<'a>(app: &'a App, snapshot: &'a Snapshot, width: f32) -> Element<'a, Msg> {
    let user = &snapshot.me;
    let metrics: Element<'_, Msg> = if width < 740.0 {
        surface(
            row![
                compact_metric(
                    "Memory",
                    user.committed.memory_bytes / GIB,
                    user.grant.memory_bytes / GIB,
                    "GiB"
                ),
                compact_metric(
                    "Dedicated CPU",
                    user.committed.cpu_cores,
                    user.grant.cpu_cores,
                    "cores"
                ),
                compact_metric(
                    "Disk backing",
                    user.committed.disk_bytes / GIB,
                    user.grant.disk_bytes / GIB,
                    "GiB"
                )
            ]
            .spacing(20),
            false,
        )
        .into()
    } else {
        row![
            metric(
                "Memory",
                user.committed.memory_bytes / GIB,
                user.grant.memory_bytes / GIB,
                "GiB"
            ),
            metric(
                "Dedicated CPU",
                user.committed.cpu_cores,
                user.grant.cpu_cores,
                "cores"
            ),
            metric(
                "Disk backing",
                user.committed.disk_bytes / GIB,
                user.grant.disk_bytes / GIB,
                "GiB"
            )
        ]
        .spacing(12)
        .into()
    };
    let mut body = column![
        metrics,
        row![
            heading("Your machines"),
            horizontal_space(),
            copy(format!("{} total", snapshot.vms.len()))
        ]
        .align_y(Alignment::Center)
    ]
    .spacing(24);
    if snapshot.vms.is_empty() {
        body=body.push(surface(column![container(glyph("vm")).padding(12),heading("A workspace of your own"),copy("Create a VM for your projects. Your files and T3 server stay here when your Mac disconnects."),button("Create your first VM").on_press(Msg::Open(Panel::Create))].spacing(16).align_x(Alignment::Center),false));
    } else {
        let columns = if width >= 840.0 { 2 } else { 1 };
        for chunk in snapshot.vms.chunks(columns) {
            let mut line = row![].spacing(16);
            for vm in chunk {
                line = line.push(vm_card(app, vm));
            }
            if chunk.len() < columns {
                line = line.push(horizontal_space().width(Length::Fill));
            }
            body = body.push(line);
        }
    }
    body.push(copy(
        "Stopped VMs keep their allocations. Manage a machine to change resources or attach a GPU.",
    ))
    .into()
}
fn vm_card<'a>(app: &'a App, vm: &'a Vm) -> Element<'a, Msg> {
    let ready = vm.observed == "running" && vm.ssh_host_key.is_some();
    let mut content = column![
        row![
            container(glyph("vm"))
                .padding(10)
                .style(|theme: &Theme| container::Style {
                    background: Some(theme.colors.secondary_container.into()),
                    text_color: Some(theme.colors.on_secondary_container),
                    border: Border {
                        radius: 14.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            horizontal_space(),
            badge(
                state_label(vm),
                vm.observed == "running" && vm.error.is_none()
            )
        ]
        .align_y(Alignment::Center),
        typography(&vm.spec.name, Type::TitleLarge).width(Length::Fill),
        copy("Ubuntu · development VM"),
        row![
            vm_stat(format!("{} GiB", vm.spec.memory_gib), "Memory"),
            vm_stat(
                format!(
                    "{} core{}",
                    vm.spec.cores,
                    if vm.spec.cores == 1 { "" } else { "s" }
                ),
                "Dedicated CPU"
            ),
            vm_stat(format!("{} GiB", vm.spec.disk_gib), "Disk")
        ]
        .spacing(16),
    ]
    .spacing(16);
    if let Some(error) = &vm.error {
        content = content.push(copy(error.strip_prefix("pending: ").unwrap_or(error)));
    }
    if vm.gpu.is_some() {
        content = content.push(badge("GPU reserved", true));
    }
    content = content.push(
        row![
            button("Open terminal")
                .on_press(Msg::Ssh(vm.id, false))
                .disabled(app.busy || !ready),
            secondary("Set up T3", Msg::Open(Panel::T3(vm.id)))
                .disabled(app.busy || vm.ssh_host_key.is_none()),
            quiet("Manage", Msg::Open(Panel::Vm(vm.id)))
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap(),
    );
    surface(content, false).into()
}
fn vm_stat(value: String, label: &'static str) -> Element<'static, Msg> {
    column![typography(value, Type::TitleMedium), copy(label)]
        .spacing(4)
        .width(Length::Fill)
        .into()
}
fn host<'a>(app: &'a App, s: &'a Snapshot) -> Element<'a, Msg> {
    let i = &s.inventory;
    let healthy = i.pool_health == "ONLINE"
        && i.warnings.is_empty()
        && i.iommu.as_ref().is_none_or(|r| r.state == "ready");
    let mut body = column![surface(
        row![
            column![
                copy("HOST STATUS"),
                typography(
                    if healthy {
                        "Host is healthy"
                    } else {
                        "Review host setup"
                    },
                    Type::HeadlineMedium
                ),
                copy(format!(
                    "{} · {} physical cores · {} GiB RAM",
                    i.hostname,
                    i.physical_cores,
                    i.memory_bytes / GIB
                ))
            ]
            .spacing(12)
            .width(Length::Fill),
            badge(if healthy { "Healthy" } else { "Check host" }, healthy)
        ]
        .spacing(16)
        .align_y(Alignment::Center),
        true
    )]
    .spacing(20);
    for warning in &i.warnings {
        body = body.push(message(warning, true));
    }
    body = body.push(surface(
        column![
            row![
                heading("Storage"),
                horizontal_space(),
                badge(&i.pool_health, i.pool_health == "ONLINE")
            ]
            .align_y(Alignment::Center),
            typography(
                format!("{} GiB available", i.pool_available_bytes / GIB),
                Type::HeadlineMedium
            ),
            copy(format!(
                "Pool {} · reserved physical backing for your users",
                i.pool
            ))
        ]
        .spacing(14),
        false,
    ));
    let mut hardware = column![
        row![
            heading("GPU passthrough"),
            horizontal_space(),
            badge(
                i.iommu
                    .as_ref()
                    .map(|r| if r.state == "ready" {
                        "IOMMU ready"
                    } else {
                        "Setup needed"
                    })
                    .unwrap_or("Unknown"),
                i.iommu.as_ref().is_some_and(|r| r.state == "ready")
            )
        ]
        .align_y(Alignment::Center)
    ]
    .spacing(16);
    if i.gpus.is_empty() {
        hardware = hardware.push(copy("No supported GPU detected."));
    }
    for gpu in &i.gpus {
        hardware = hardware
            .push(typography(gpu_name(&gpu.model), Type::TitleMedium))
            .push(copy(if gpu.readiness.state == "ready" {
                "Available for an eligible VM. Assign it from the machine's Manage dialog."
            } else if gpu.readiness.state == "assigned" {
                "Exclusively reserved to a VM, including while that VM is stopped."
            } else {
                "Review the hardware setup before assigning this device."
            }))
            .push(copy(gpu.telemetry.as_deref().unwrap_or(
                "Utilization is unavailable until a supported guest driver responds.",
            )));
        for step in &gpu.readiness.next_steps {
            hardware = hardware.push(copy(step));
        }
    }
    if let Some(iommu) = &i.iommu {
        for step in &iommu.next_steps {
            hardware = hardware.push(copy(step));
        }
    }
    hardware = hardware.push(quiet(
        if app.hardware_details {
            "Hide hardware details"
        } else {
            "Show hardware details"
        },
        Msg::ToggleHardware,
    ));
    if app.hardware_details {
        hardware = hardware.push(copy(&i.cpu_model));
        if let Some(iommu) = &i.iommu {
            for evidence in &iommu.evidence {
                hardware = hardware.push(copy(evidence));
            }
        }
        for gpu in &i.gpus {
            hardware = hardware.push(copy(format!(
                "PCI {} · driver {}",
                gpu.pci,
                gpu.driver.as_deref().unwrap_or("unbound")
            )));
            for line in &gpu.readiness.evidence {
                hardware = hardware.push(copy(line));
            }
        }
    }
    body = body.push(surface(hardware, false)).push(surface(
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
    ));
    body.into()
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
            _ => "In progress",
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
        container(glyph("cloud")).padding(12),
        typography(
            if connecting {
                "Connecting to your cloud"
            } else {
                "Your cloud. Your workspace."
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
fn panel_view<'a>(app: &'a App, panel: &'a Panel) -> iced_m3::Dialog<'a, Msg> {
    let mut content = column![].spacing(20);
    let mut actions = row![quiet("Cancel", Msg::Close).disabled(app.busy)].spacing(8);
    match panel {
        Panel::Create | Panel::Resize(_) => {
            let resize = matches!(panel, Panel::Resize(_));
            content = content
                .push(heading(if resize {
                    "Resize machine"
                } else {
                    "Create a workspace"
                }))
                .push(copy(if resize {
                    "The VM will restart to apply these sizes. Disk capacity can only grow."
                } else {
                    "An isolated Ubuntu VM with persistent storage and your T3 server."
                }));
            if !resize {
                content = content.push(app.input("Machine name", Field::Name));
            }
            content=content.push(row![app.input("Memory · GiB",Field::Ram),app.input("Physical cores",Field::Cores),app.input("Disk · GiB",Field::Disk)].spacing(12))
                .push(copy("Whole physical cores include SMT siblings. Disk backing includes 2 GiB of additional overhead on this host."));
            if !resize {
                content = content
                    .push(app.input("Guest SSH public key", Field::Key))
                    .push(copy(
                        "Only a public key. Your private key stays on this Mac.",
                    ));
            }
            let msg = if let Panel::Resize(id) = panel {
                Msg::Resize(*id)
            } else {
                Msg::Create
            };
            actions = actions.push(
                button(if app.busy {
                    "Submitting…"
                } else if resize {
                    "Apply & restart"
                } else {
                    "Create VM"
                })
                .on_press(msg)
                .disabled(app.busy),
            );
        }
        Panel::Vm(id) => {
            if let Some(vm) = app
                .snapshot
                .as_ref()
                .and_then(|s| s.vms.iter().find(|v| v.id == *id))
            {
                content = content
                    .push(typography(&vm.spec.name, Type::TitleLarge).width(Length::Fill))
                    .push(badge(state_label(vm), vm.observed == "running"))
                    .push(copy(format!(
                        "{} GiB RAM · {} dedicated cores · {} GiB disk",
                        vm.spec.memory_gib, vm.spec.cores, vm.spec.disk_gib
                    )));
                content = content.push(
                    row![
                        secondary(
                            if vm.desired == Lifecycle::Running {
                                "Stop VM"
                            } else {
                                "Start VM"
                            },
                            Msg::Change(Mutation::State {
                                vm_id: *id,
                                desired: if vm.desired == Lifecycle::Running {
                                    Lifecycle::Stopped
                                } else {
                                    Lifecycle::Running
                                }
                            })
                        )
                        .disabled(app.busy),
                        secondary("Resize", Msg::Open(Panel::Resize(*id))).disabled(app.busy)
                    ]
                    .spacing(8),
                );
                content=content.push(heading("GPU")).push(copy("Assignments are exclusive and remain reserved while the VM is stopped. Changes can restart this machine."));
                if let Some(pci) = &vm.gpu {
                    content = content.push(copy(format!("Reserved device: {pci}"))).push(
                        secondary(
                            "Detach GPU",
                            Msg::Change(Mutation::Gpu {
                                vm_id: *id,
                                pci: None,
                            }),
                        )
                        .disabled(app.busy),
                    );
                } else if let Some(snapshot) = &app.snapshot {
                    if !snapshot.me.gpu_allowed && snapshot.me.role != Role::Admin {
                        content =
                            content.push(copy("Ask your administrator to enable GPU access."));
                    } else {
                        for gpu in &snapshot.inventory.gpus {
                            content = content
                                .push(typography(gpu_name(&gpu.model), Type::TitleMedium))
                                .push(
                                    secondary(
                                        "Attach GPU",
                                        Msg::Change(Mutation::Gpu {
                                            vm_id: *id,
                                            pci: Some(gpu.pci.clone()),
                                        }),
                                    )
                                    .disabled(
                                        app.busy
                                            || gpu.readiness.state != "ready"
                                            || vm.spec.memory_gib < 4,
                                    ),
                                );
                        }
                    }
                    if vm.spec.memory_gib < 4 {
                        content = content
                            .push(copy("Resize to at least 4 GiB RAM before attaching a GPU."));
                    }
                }
                content = content.push(quiet(
                    if app.identity_details {
                        "Hide machine identity"
                    } else {
                        "Show machine identity"
                    },
                    Msg::ToggleIdentity,
                ));
                if app.identity_details {
                    content = content
                        .push(copy(vm.id.to_string()))
                        .push(copy("SSH host-key trust is pinned to this identity."));
                }
                content = content.push(
                    button("Delete VM")
                        .variant(Variant::Text)
                        .palette(|t| (Color::TRANSPARENT, t.colors.error))
                        .on_press(Msg::Delete(*id))
                        .disabled(app.busy),
                );
                actions = row![quiet("Done", Msg::Close).disabled(app.busy)];
            }
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
            content=content.push(app.input("Memory grant · GiB",Field::RamGrant)).push(app.input("Physical core grant",Field::CoreGrant)).push(app.input("Disk backing grant · GiB",Field::DiskGrant))
                .push(iced_m3::switch(app.gpu).label("Allow GPU assignment").on_toggle(|_|Msg::ToggleGpu))
                .push(copy(if invite{"The invitation expires after 24 hours and can enroll one device."}else{"Reducing a grant blocks further growth. Existing VMs and GPU assignments keep running."}));
            actions = actions.push(
                button(if invite {
                    "Create invitation"
                } else {
                    "Save grant"
                })
                .on_press(if invite { Msg::Invite } else { Msg::Grant })
                .disabled(app.busy || (invite && app.get(Field::User).trim().is_empty())),
            );
        }
        Panel::T3(id) => {
            content=content.push(heading("Connect T3 Code")).push(copy("Your projects, provider credentials, and agents run inside this VM."))
                .push(copy("1. Set up and copy this machine's SSH alias."))
                .push(copy("2. In T3 Code, open Settings → Environments → Add environment, choose SSH, and paste the alias."))
                .push(copy("3. Sign into your provider in that remote environment."))
                .push(copy("This adds a managed Include to your Mac's SSH config. Existing entries are preserved, and host-key checking stays enabled."));
            actions = actions.push(
                button("Set up & copy alias")
                    .on_press(Msg::Ssh(*id, true))
                    .disabled(app.busy),
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
        Panel::Revoke(peer) => {
            content=content.push(heading("Revoke this device?")).push(copy(short(peer))).push(copy("Its active SSH tunnels will close, and it will no longer be able to connect. Enrolling it again requires a fresh device key."));
            actions =
                actions.push(danger("Revoke access", Msg::Revoke(peer.clone())).disabled(app.busy));
        }
    }
    if let Some(error) = &app.error {
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
