use super::*;
use iced::widget::{column, row};

pub(super) fn banner<'a>(app: &App, s: &Snapshot) -> Option<Element<'a, Msg>> {
    let health = s.memory_health.as_ref()?;
    if health.condition == MemoryCondition::Healthy {
        return None;
    }
    let mut body = column![typography(health.condition.title(), Type::TitleSmall)].spacing(6);
    let explanation = if app.snapshot_unavailable {
        "Last verified memory warning. Reconnect to check whether recovery has completed."
    } else {
        health.condition.explanation()
    };
    body = body.push(typography(explanation, Type::BodySmall));
    let action: Element<'a, Msg> = if s.me.role == Role::Admin {
        quiet("Review host", Msg::ReviewHostMemory).into()
    } else {
        typography("Ask your administrator", Type::LabelMedium).into()
    };
    Some(
        container(
            row![body.width(Length::Fill), action]
                .spacing(12)
                .align_y(Alignment::Center),
        )
        .padding(16)
        .width(Length::Fill)
        .id("host-memory-warning")
        .style(|t: &Theme| container::Style {
            background: Some(t.colors.error_container.into()),
            text_color: Some(t.colors.on_error_container),
            border: Border {
                radius: 16.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into(),
    )
}

pub(super) fn host_recovery<'a>(app: &App, s: &'a Snapshot) -> Option<Element<'a, Msg>> {
    let h = s.memory_health.as_ref()?;
    if h.condition == MemoryCondition::Healthy || s.me.role != Role::Admin {
        return None;
    }
    let mut body = column![heading("Memory recovery"), copy(h.condition.explanation())].spacing(12);
    if let Some(d) = &h.details {
        body = body.push(copy(format!(
            "VM budget: {:.1} GiB · promises: {:.1} GiB · held: {:.1} GiB",
            d.capacity_bytes as f64 / GIB as f64,
            d.promised_bytes as f64 / GIB as f64,
            d.held_bytes as f64 / GIB as f64
        )));
    }
    body = body.push(copy(match h.condition {
        MemoryCondition::AllocationsExceedCapacity => "Keep the VMs you want to resume. Stop others to confirm cleanup and release their compute, then edit them while stopped. Lowering a grant alone does not release a VM’s claim.",
        MemoryCondition::PromisesExceedCapacity => "Lower unused grants until promises fit the budget, or restore physical capacity. Existing VM claims remain protected.",
        MemoryCondition::ArcNotEnforced => "Open reserves and cache, then explicitly reapply the backed cache policy. Existing VM claims are retained; reducing grants does not repair the cache limit.",
        _ => "Restore host connectivity and verify its policy before changing allocations. Refresh to check recovery; claims remain protected.",
    }));
    for vm in s.vms.iter().filter(|v| h.waiting(v)) {
        let owner = s
            .users
            .iter()
            .find(|u| u.id == vm.owner)
            .map(|u| u.name.as_str())
            .unwrap_or("Owner unavailable");
        body = body.push(
            row![
                column![
                    typography(vm.spec.name.clone(), Type::TitleSmall),
                    copy(format!(
                        "{owner} · {} GiB held",
                        vm.resources.memory_bytes / GIB
                    ))
                ]
                .spacing(4)
                .width(Length::Fill),
                quiet("Review VM", Msg::OpenWorkspace(vm.id)),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }
    let mut actions = row![quiet("Review grants", Msg::Navigate(Page::People))].spacing(8);
    if s.host_reserves.is_some() {
        actions = actions.push(
            quiet("Edit reserves and cache", Msg::Open(Panel::HostReserves))
                .disabled(app.busy || app.snapshot_unavailable),
        );
        if s.host_reserves
            .as_ref()
            .and_then(|v| v.memory.as_ref())
            .is_some_and(|m| m.safe_bytes != m.reviewed_bytes)
        {
            actions = actions.push(
                quiet(
                    "Review detected RAM change",
                    Msg::Open(Panel::ReviewMemoryCapacity),
                )
                .disabled(app.busy || app.snapshot_unavailable),
            );
        }
    }
    Some(surface(body.push(actions.wrap()), false).padding(20).into())
}

pub(super) fn vm_recovery<'a>(app: &App, s: &Snapshot, vm: &Vm) -> Option<Element<'a, Msg>> {
    let h = s.memory_health.as_ref().filter(|h| h.waiting(vm))?;
    let mut body = column![heading("Startup is waiting for host recovery"), copy(h.condition.explanation()),
        copy("This VM’s disk and existing compute claims are retained. Stop it to release compute after cleanup.")].spacing(12);
    let mut actions = row![].spacing(8);
    if s.me.role == Role::Admin {
        actions = actions.push(quiet("Review host", Msg::ReviewHostMemory));
    } else {
        body = body.push(copy("Ask your administrator to review the host. You can stop this VM to release its compute after cleanup."));
    }
    if lifecycle::unfinished(s, vm).is_none() {
        actions = actions.push(
            secondary("Stop and release compute…", Msg::Open(Panel::Stop(vm.id)))
                .disabled(app.busy || app.snapshot_unavailable),
        );
    }
    Some(surface(body.push(actions.wrap()), false).padding(20).into())
}
