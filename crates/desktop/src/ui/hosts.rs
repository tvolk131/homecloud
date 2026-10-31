use super::*;
use crate::{
    HostTab,
    allocation::{Allocation, ResourceKind},
};
use iced::widget::{column, row};
use iced_m3::tabs::{Tab, tabs};
mod allocations;
mod budget;
mod storage;
use uuid::Uuid;

// Stable identity colors, generated as M3 tonal pairs in the current theme.
// Names and exact quantities remain visible; color is never the only key.
fn owner_colors(id: Uuid, theme: &Theme) -> iced_m3::theme::ColorScheme {
    let seeds = [
        (109, 78, 181),
        (0, 124, 133),
        (48, 101, 182),
        (155, 103, 0),
        (166, 65, 111),
        (73, 119, 52),
    ];
    let hash = id
        .as_bytes()
        .iter()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(*b as usize));
    let (r, g, b) = seeds[hash % seeds.len()];
    Theme::from_accent(Color::from_rgb8(r, g, b), theme.is_dark()).colors
}
fn swatch<'a>(owner: Option<Uuid>, soft: bool) -> Element<'a, Msg> {
    container(widget::space().width(16).height(12))
        .style(move |theme: &Theme| container::Style {
            background: Some(
                match owner {
                    Some(id) => {
                        let c = owner_colors(id, theme);
                        if soft { c.primary_container } else { c.primary }
                    }
                    None => theme.colors.surface_container_highest,
                }
                .into(),
            ),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

pub(super) fn hostname(s: &Snapshot) -> &str {
    if s.inventory.hostname.is_empty() {
        "Connected host"
    } else {
        &s.inventory.hostname
    }
}

fn summary_resource(s: &Snapshot, kind: ResourceKind) -> Element<'static, Msg> {
    let model = Allocation::new(s, kind);
    let backed = if s.me.role == Role::Admin {
        model.backed()
    } else {
        model.held()
    };
    let fraction = if model.capacity == 0 {
        if backed == 0 { 0.0 } else { 1.0 }
    } else {
        (backed as f32 / model.capacity as f32).min(1.0)
    };
    column![
        copy(kind.label()),
        typography(
            format!("{} / {}", kind.format(backed), kind.format(model.capacity)),
            Type::TitleMedium
        ),
        iced_m3::linear_progress(fraction),
    ]
    .spacing(8)
    .width(Length::Fill)
    .into()
}

/// A directory entry for the controller currently connected to this client.
/// Fleet discovery/aggregation is separate from selecting a host's detail view.
pub(super) fn directory<'a>(app: &App, s: &'a Snapshot) -> Element<'a, Msg> {
    let physical = if s.inventory.physical_cores == 0 || s.inventory.memory_bytes == 0 {
        "Hardware inventory unavailable".into()
    } else {
        format!(
            "{} physical cores · {} RAM · {} GPU{}",
            s.inventory.physical_cores,
            ResourceKind::Memory.format(s.inventory.memory_bytes as u128),
            s.inventory.gpus.len(),
            if s.inventory.gpus.len() == 1 { "" } else { "s" }
        )
    };
    let header = container(
        iced_m3::Button::new(
            row![
                column![typography(hostname(s), Type::TitleLarge), copy(physical)]
                    .spacing(6)
                    .width(Length::Fill),
                typography("›", Type::HeadlineMedium),
            ]
            .spacing(16)
            .align_y(Alignment::Center),
        )
        .variant(Variant::Text)
        .width(Length::Fill)
        .padding([8, 12])
        .radius(12.0)
        .on_press(Msg::Navigate(Page::Host)),
    )
    .id("host-open");
    let status = if app.snapshot_unavailable {
        "Status unavailable"
    } else if s
        .memory_health
        .as_ref()
        .is_some_and(|h| h.condition != MemoryCondition::Healthy)
    {
        "Check host"
    } else if s.inventory.pool_health == "ONLINE" {
        "Pool online"
    } else {
        "Check host"
    };
    column![
        copy("1 host"),
        surface(
            column![
                header,
                row![
                    copy(if s.me.role == Role::Admin {
                        "Backed / VM capacity"
                    } else {
                        "Allocated / your grant"
                    }),
                    horizontal_space(),
                    badge(status, status == "Pool online")
                ]
                .spacing(12)
                .align_y(Alignment::Center),
                row![
                    summary_resource(s, ResourceKind::Memory),
                    summary_resource(s, ResourceKind::Cpu),
                    summary_resource(s, ResourceKind::Storage)
                ]
                .spacing(24),
            ]
            .spacing(20),
            false
        )
        .padding(20),
    ]
    .spacing(16)
    .into()
}

pub(super) fn navigation(app: &App) -> Element<'static, Msg> {
    let mut items = vec![
        Tab::new(HostTab::Overview, "Overview"),
        Tab::new(HostTab::Allocations, "Allocations"),
        Tab::new(HostTab::Hardware, "Hardware"),
    ];
    if app
        .snapshot
        .as_ref()
        .is_some_and(|s| s.me.role == Role::Admin)
    {
        items.push(Tab::new(HostTab::Storage, "Disks & ZFS"));
    }
    tabs(items, Some(app.host_tab))
        .on_select(Msg::HostTab)
        .into()
}

pub(super) fn view<'a>(app: &'a App, s: &'a Snapshot, width: f32) -> Element<'a, Msg> {
    let mut body = column![].spacing(20);
    for warning in &s.inventory.warnings {
        body = body.push(message(warning, true));
    }
    body.push(match app.host_tab {
        HostTab::Overview => overview(app, s, width),
        HostTab::Allocations => allocations::view(app, s),
        HostTab::Hardware => hardware(app, s, width),
        HostTab::Storage => storage::view(app, s),
    })
    .into()
}

fn overview<'a>(app: &'a App, s: &'a Snapshot, width: f32) -> Element<'a, Msg> {
    let admin = s.me.role == Role::Admin;
    let model = Allocation::new(s, ResourceKind::Memory);
    let mut owners =
        row![quiet("Everyone", Msg::HighlightOwner(None)).selected(app.host_owner.is_none())]
            .spacing(6)
            .align_y(Alignment::Center);
    if !admin {
        owners = row![copy("Your allocations on this host")];
    } else {
        for owner in &model.owners {
            let id = owner.user.id;
            owners = owners.push(
                iced_m3::Button::new(
                    row![
                        swatch(Some(id), false),
                        typography(&owner.user.name, Type::LabelLarge)
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .variant(Variant::Text)
                .selected(app.host_owner == Some(id))
                .on_press(Msg::HighlightOwner(Some(id))),
            );
        }
    }
    let legend_id = model.owners.first().map(|o| o.user.id);
    let mut legend = row![
        row![swatch(legend_id, false), copy("Allocated to VMs")]
            .spacing(6)
            .align_y(Alignment::Center),
        row![swatch(legend_id, true), copy("Unused grant")]
            .spacing(6)
            .align_y(Alignment::Center),
    ]
    .spacing(20);
    if admin {
        legend = legend.push(
            row![swatch(None, false), copy("Unissued")]
                .spacing(6)
                .align_y(Alignment::Center),
        );
    }
    let wide = width >= 840.0;
    let mut body = column![].spacing(16);
    if let Some(recovery) = memory_health::host_recovery(app, s) {
        body = body.push(recovery);
    }
    body = body.push(owners.wrap()).push(legend.wrap());
    let models = ResourceKind::ALL.map(|kind| Allocation::new(s, kind));
    if wide {
        let mut cards = row![].spacing(12);
        for model in &models {
            cards = cards.push(resource_card(app, s, model, true));
        }
        body = body.push(cards);
    } else {
        for model in &models {
            body = body.push(resource_card(app, s, model, false));
        }
    }
    if admin {
        body = body.push(budget::view(app, s));
    }
    body = body.push(gpus(s));
    body.push(copy("Allocations include pending work. Live VM utilization is unavailable; these blocks show durable backing.")).into()
}

fn resource_card(
    app: &App,
    s: &Snapshot,
    model: &Allocation<'_>,
    compact: bool,
) -> Element<'static, Msg> {
    let kind = model.kind;
    let admin = s.me.role == Role::Admin;
    let selected = if admin {
        model.selected(app.host_owner)
    } else {
        model.owners.first()
    };
    let capacity = row![
        typography(kind.label(), Type::TitleMedium),
        horizontal_space(),
        column![
            typography(kind.format(model.capacity), Type::HeadlineSmall),
            copy(if admin { "allocatable" } else { "your grant" }),
        ]
        .spacing(2)
        .align_x(Alignment::End)
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let chart = if kind == ResourceKind::Cpu && model.scale() > 0 && model.scale() <= 16 {
        core_tiles(model, app.host_owner, compact)
    } else {
        allocation_bar(model, app.host_owner, if compact { 96.0 } else { 40.0 })
    };
    let allocated = selected.map(|o| o.held).unwrap_or_else(|| model.held());
    let backed = selected
        .map(|o| o.backed())
        .unwrap_or_else(|| model.backed());
    let mut quantities = column![copy(format!(
        "{} {}",
        kind.format(allocated),
        if kind == ResourceKind::Storage {
            "VM backing"
        } else {
            "allocated"
        }
    ))]
    .push(copy(format!(
        "{} {}",
        kind.format(backed),
        selected
            .map(|o| format!("backed for {}", o.user.name))
            .unwrap_or_else(|| "backed for users".into())
    )))
    .spacing(4);
    let available = if admin {
        model.unissued()
    } else {
        model.owners[0].unused()
    };
    quantities = quantities.push(typography(
        format!(
            "{} {}",
            kind.format(available),
            if admin {
                "unissued"
            } else {
                "available in your grant"
            }
        ),
        Type::LabelLarge,
    ));
    if let Some(owner) = selected.filter(|o| o.held > o.grant) {
        quantities = quantities.push(
            typography(
                format!(
                    "{} above grant · growth paused",
                    kind.format(owner.held - owner.grant)
                ),
                Type::LabelMedium,
            )
            .style(|t: &Theme| text::Style {
                color: Some(t.colors.error),
            }),
        );
    }
    if admin && model.backed() > model.capacity {
        quantities = quantities.push(
            typography(
                format!(
                    "{} {}",
                    kind.format(model.backed() - model.capacity),
                    if admin {
                        "above managed capacity"
                    } else {
                        "above grant · growth paused"
                    }
                ),
                Type::LabelMedium,
            )
            .style(|t: &Theme| text::Style {
                color: Some(t.colors.error),
            }),
        );
    }
    surface(column![capacity, chart, quantities].spacing(16), false).into()
}

fn weight(value: u128, total: u128) -> u16 {
    ((value * u16::MAX as u128 / total.max(1))
        .max(1)
        .min(u16::MAX as u128)) as u16
}
fn mark(
    owner: Option<Uuid>,
    soft: bool,
    dim: bool,
    width: Length,
    height: f32,
) -> Element<'static, Msg> {
    container(widget::space().width(Length::Fill).height(height))
        .width(width)
        .style(move |theme: &Theme| {
            let color = owner
                .map(|id| {
                    let c = owner_colors(id, theme);
                    if soft { c.primary_container } else { c.primary }
                })
                .unwrap_or(theme.colors.surface_container_highest);
            container::Style {
                background: Some(
                    if dim {
                        mix(color, theme.colors.surface_container, 0.35)
                    } else {
                        color
                    }
                    .into(),
                ),
                border: Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}
fn mix(a: Color, b: Color, amount: f32) -> Color {
    Color::from_rgb(
        a.r * amount + b.r * (1.0 - amount),
        a.g * amount + b.g * (1.0 - amount),
        a.b * amount + b.b * (1.0 - amount),
    )
}
fn allocation_bar(
    model: &Allocation<'_>,
    selected: Option<Uuid>,
    height: f32,
) -> Element<'static, Msg> {
    let scale = model.scale();
    if scale == 0 {
        return mark(None, false, false, Length::Fill, height);
    }
    let mut bar = row![].spacing(3).height(height);
    for owner in &model.owners {
        if owner.backed() == 0 {
            continue;
        }
        let dim = selected.is_some_and(|id| id != owner.user.id);
        let mut group = row![]
            .spacing(1)
            .width(Length::FillPortion(weight(owner.backed(), scale)));
        if owner.held > 0 {
            group = group.push(mark(
                Some(owner.user.id),
                false,
                dim,
                Length::FillPortion(weight(owner.held, owner.backed())),
                height,
            ));
        }
        if owner.unused() > 0 {
            group = group.push(mark(
                Some(owner.user.id),
                true,
                dim,
                Length::FillPortion(weight(owner.unused(), owner.backed())),
                height,
            ));
        }
        bar = bar.push(group);
    }
    if model.unissued() > 0 {
        bar = bar.push(mark(
            None,
            false,
            false,
            Length::FillPortion(weight(model.unissued(), scale)),
            height,
        ));
    }
    bar.into()
}
fn core_tiles(
    model: &Allocation<'_>,
    selected: Option<Uuid>,
    compact: bool,
) -> Element<'static, Msg> {
    let mut tiles = vec![];
    for owner in &model.owners {
        for n in 0..owner.backed() {
            let soft = n >= owner.held;
            tiles.push((Some(owner.user.id), owner.user.name.clone(), soft));
        }
    }
    for _ in 0..model.unissued() {
        tiles.push((None, "Unissued".into(), false));
    }
    let per_row = if compact { 2 } else { 8 };
    let mut grid = column![].spacing(4);
    let count = tiles.len();
    for chunk in tiles.chunks(per_row) {
        let mut line = row![].spacing(4);
        for (id, name, soft) in chunk {
            let id = *id;
            let soft = *soft;
            let dim = selected.is_some_and(|selection| id.is_some_and(|id| selection != id));
            let label = if compact || count <= 6 {
                name.chars().take(9).collect::<String>()
            } else {
                name.chars().next().unwrap_or('–').to_string()
            };
            line = line.push(
                container(
                    column![
                        typography(label, Type::LabelLarge),
                        typography(
                            if id.is_none() {
                                "Free"
                            } else if soft {
                                "Grant"
                            } else {
                                "VM"
                            },
                            Type::LabelSmall
                        )
                    ]
                    .spacing(2)
                    .align_x(Alignment::Center),
                )
                .padding(6)
                .center_x(Length::Fill)
                .center_y(46)
                .style(move |theme: &Theme| {
                    let c = id.map(|id| owner_colors(id, theme));
                    let (fill, on) = if let Some(c) = c {
                        if soft {
                            (c.primary_container, c.on_primary_container)
                        } else {
                            (c.primary, c.on_primary)
                        }
                    } else {
                        (
                            theme.colors.surface_container_highest,
                            theme.colors.on_surface_variant,
                        )
                    };
                    container::Style {
                        background: Some(
                            if dim {
                                mix(fill, theme.colors.surface_container, 0.35)
                            } else {
                                fill
                            }
                            .into(),
                        ),
                        text_color: Some(if dim { theme.colors.on_surface } else { on }),
                        border: Border {
                            radius: 9.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                }),
            );
        }
        // Keep the last row's tiles at the same width as the preceding rows.
        if count > per_row {
            for _ in chunk.len()..per_row {
                line = line.push(horizontal_space());
            }
        }
        grid = grid.push(line);
    }
    grid.into()
}

struct GpuSummary {
    status: &'static str,
    detail: String,
    available: bool,
    needs_setup: bool,
}
fn gpu_summary(s: &Snapshot, gpu: &Gpu) -> GpuSummary {
    if let Some(vm) = s.vms.iter().find(|vm| vm.claims_gpu(&gpu.pci)) {
        let owner = if vm.owner == s.me.id {
            Some(s.me.name.as_str())
        } else if s.me.role == Role::Admin {
            s.users
                .iter()
                .find(|u| u.id == vm.owner)
                .map(|u| u.name.as_str())
        } else {
            None
        };
        let detail = owner
            .map(|owner| format!("{} · {owner}", vm.spec.name))
            .unwrap_or_else(|| "Reserved by another VM".into());
        let review = vm
            .error
            .as_ref()
            .is_some_and(|e| !e.starts_with("pending:"));
        return GpuSummary {
            status: if review {
                "Claim held · review needed"
            } else if vm.desired != Lifecycle::Running {
                "Releasing"
            } else if vm.observed != "running" {
                "Reserved · starting"
            } else {
                "Reserved"
            },
            detail,
            available: false,
            needs_setup: false,
        };
    }
    if gpu.assigned_vm.is_some() || gpu.readiness.state == "assigned" {
        return GpuSummary {
            status: "In use",
            detail: "Observed attachment; unavailable for assignment.".into(),
            available: false,
            needs_setup: false,
        };
    }
    let ready = gpu.readiness.state == "ready"
        && s.inventory
            .iommu
            .as_ref()
            .is_some_and(|i| i.state == "ready");
    GpuSummary {
        status: if ready { "Available" } else { "Setup needed" },
        detail: if ready {
            "Choose GPUs in VM details → Edit VM.".into()
        } else {
            "Review passthrough readiness in Hardware.".into()
        },
        available: ready,
        needs_setup: !ready,
    }
}
fn gpus(s: &Snapshot) -> Element<'_, Msg> {
    let mut content = column![heading("GPUs")].spacing(14);
    if s.inventory.gpus.is_empty() {
        content = content.push(copy("No GPU reported by host inventory."));
    }
    for gpu in &s.inventory.gpus {
        let summary = gpu_summary(s, gpu);
        content = content.push(
            row![
                column![
                    typography(gpu_name(&gpu.model), Type::TitleMedium),
                    copy(summary.detail)
                ]
                .spacing(5)
                .width(Length::Fill),
                badge(summary.status, summary.available)
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
        if summary.needs_setup
            || s.inventory
                .iommu
                .as_ref()
                .is_none_or(|i| i.state != "ready")
        {
            content = content.push(quiet("Review GPU setup", Msg::HostTab(HostTab::Hardware)));
        }
    }
    surface(content, false).into()
}

fn hardware<'a>(app: &'a App, s: &'a Snapshot, _width: f32) -> Element<'a, Msg> {
    let i = &s.inventory;
    let mut body=column![
        surface(column![heading("Processor & memory"),
            typography(if i.cpu_model.is_empty() {"Inventory unavailable"} else {&i.cpu_model},Type::TitleMedium),
            copy(format!("{} physical cores · {} installed RAM",i.physical_cores,ResourceKind::Memory.format(i.memory_bytes as u128))),
            copy(format!("Managed: {} CPU · {} memory",ResourceKind::Cpu.format(s.capacity.cpu_cores as u128),ResourceKind::Memory.format(s.capacity.memory_bytes as u128))),
            copy("Managed capacity is configured by the operator. The remainder can include host reserves and unmanaged workloads.")
        ].spacing(12),false),
        surface(column![row![heading("Storage pool"),horizontal_space(),badge(&i.pool_health,i.pool_health=="ONLINE")].spacing(16).align_y(Alignment::Center),
            typography(format!("Pool {}",i.pool),Type::TitleMedium),
            copy(format!("{} reported available by ZFS",ResourceKind::Storage.format(i.pool_available_bytes as u128))),
            copy(format!("{} managed backing capacity",ResourceKind::Storage.format(s.capacity.disk_bytes as u128))),
            copy("ZFS available space is not capacity available to grant. User guarantees remain backed even when guest disks are empty.")
        ].spacing(12),false),
    ].spacing(16);
    let mut graphics = column![
        row![
            heading("Graphics & passthrough"),
            horizontal_space(),
            badge(
                if i.iommu.as_ref().is_some_and(|r| r.state == "ready") {
                    "IOMMU ready"
                } else {
                    "Setup needed"
                },
                i.iommu.as_ref().is_some_and(|r| r.state == "ready")
            )
        ]
        .spacing(12)
        .align_y(Alignment::Center)
    ]
    .spacing(14);
    for gpu in &i.gpus {
        graphics = graphics
            .push(typography(gpu_name(&gpu.model), Type::TitleMedium))
            .push(copy(gpu.telemetry.as_deref().unwrap_or(
                "GPU utilization and guest driver health are unavailable.",
            )));
        for step in &gpu.readiness.next_steps {
            graphics = graphics.push(copy(step));
        }
    }
    if let Some(iommu) = &i.iommu {
        for step in &iommu.next_steps {
            graphics = graphics.push(copy(step));
        }
    }
    graphics = graphics.push(quiet(
        if app.hardware_details {
            "Hide passthrough diagnostics"
        } else {
            "Show passthrough diagnostics"
        },
        Msg::ToggleHardware,
    ));
    if app.hardware_details {
        if let Some(iommu) = &i.iommu {
            for evidence in &iommu.evidence {
                graphics = graphics.push(copy(evidence));
            }
        }
        for gpu in &i.gpus {
            graphics = graphics.push(copy(format!(
                "PCI {} · driver {} · IOMMU group {}",
                gpu.pci,
                gpu.driver.as_deref().unwrap_or("unbound"),
                gpu.iommu_group
                    .map(|g| g.to_string())
                    .unwrap_or_else(|| "unknown".into())
            )));
            if gpu.host_display {
                graphics = graphics.push(copy(
                    "This GPU provides the host display. Passthrough may blank the host monitor.",
                ));
            }
            for evidence in &gpu.readiness.evidence {
                graphics = graphics.push(copy(evidence));
            }
        }
    }
    body = body.push(surface(graphics, false));
    body.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_tests::{fixture, id};
    #[test]
    fn durable_gpu_claim_wins_over_ready_inventory_until_cleanup() {
        let mut app = fixture();
        let s = app.snapshot.as_mut().unwrap();
        s.vms[0].gpus = vec!["0000:01:00.0".to_uppercase()];
        s.vms[0].desired = Lifecycle::Stopped;
        let summary = gpu_summary(s, &s.inventory.gpus[0]);
        assert_eq!(summary.status, "Releasing");
        assert!(!summary.available);
        s.vms[0].gpus = vec![];
        s.vms[0].preferred_gpus = vec!["0000:01:00.0".into()];
        assert!(gpu_summary(s, &s.inventory.gpus[0]).available);
        s.vms[0].pending_gpus = vec!["0000:01:00.0".into()];
        assert!(!gpu_summary(s, &s.inventory.gpus[0]).available);
        s.vms[0].pending_gpus = vec![];
        s.inventory.gpus[0].assigned_vm = Some(id(900));
        assert!(!gpu_summary(s, &s.inventory.gpus[0]).available);
    }
}
