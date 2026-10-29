use super::*;
use crate::{allocation::ResourceKind, reserves::FIELDS};
use iced::widget::{column, row};

pub(super) fn form(app: &App) -> (Element<'_, Msg>, Element<'_, Msg>) {
    let Some(draft) = app.reserve_draft.as_ref() else {
        return (
            copy("Host capacity unavailable."),
            quiet("Close", Msg::Close).into(),
        );
    };
    let values = draft.values(app);
    let arc = draft.arc(app);
    let cache_repair = app
        .snapshot
        .as_ref()
        .and_then(|s| s.host_reserves.as_ref())
        .and_then(|v| v.memory.as_ref())
        .is_some_and(|m| !m.arc_ready);
    let changed = cache_repair
        || values.as_ref().is_ok_and(|v| *v != draft.current)
        || arc.as_ref().is_ok_and(|a| *a != draft.current_arc);
    let helper = match &values {
        Ok(total) => match total
            .checked_sub(draft.minimum)
            .and_then(|extra| draft.envelope.checked_sub(extra))
        {
            Ok(cap) => format!(
                "For VMs: {} memory · {} CPU · {} storage.",
                ResourceKind::Memory.format(cap.memory_bytes as u128),
                ResourceKind::Cpu.format(cap.cpu_cores as u128),
                ResourceKind::Storage.format(cap.disk_bytes as u128)
            ),
            Err(error) => error,
        },
        Err(error) => error.to_string(),
    };
    let mut body = column![
        heading("Edit host reserves"),
        copy("Set total system reserves while keeping user grants and VM commitments protected."),
        resource_controls::controls_with(app, FIELDS, draft.bounds),
        resource_controls::supporting(helper, values.is_err()),
    ]
    .spacing(12);
    if draft.current_arc.is_some() {
        if let Some(policy) = draft.memory_policy {
            let allowance = draft
                .values(app)
                .ok()
                .and_then(|v| policy.allowance(v.memory_bytes).ok())
                .unwrap_or(0);
            body = body.push(copy(format!(
                "Within system reserves: {} operating floor · up to {} cache.",
                ResourceKind::Memory.format(policy.operating_floor_bytes as u128),
                ResourceKind::Memory.format(allowance as u128)
            )));
        }
        let disabled = app.busy || app.snapshot_unavailable || draft.memory_policy.is_none();
        let bound = draft.cache_bounds(app);
        let value = app
            .get(Field::ArcLimit)
            .parse::<u64>()
            .unwrap_or(bound.min)
            .clamp(bound.min, bound.max.max(bound.min));
        let slider = iced_m3::slider(
            bound.min as f32..=bound.max.max(bound.min) as f32,
            value as f32,
        )
        .step(1.0)
        .ticks(bound.max.saturating_sub(bound.min) <= 16)
        .on_change(|v| Msg::ResourceValue(Field::ArcLimit, v.round() as u64))
        .disabled(disabled || draft.automatic || bound.max <= bound.min);
        body=body.push(typography("ZFS cache",Type::TitleMedium))
            .push(iced_m3::checkbox(draft.automatic).label("Automatic (Homecloud)").on_toggle(Msg::ReserveAutomatic)
                .disabled(disabled || (!draft.automatic_supported && !draft.automatic)))
            .push(row![container(column![typography("Custom limit",Type::TitleSmall),copy(format!("{}–{} GiB",bound.min,bound.max))].spacing(4)).width(96),
                container(slider).id("resource-slider-arc").width(Length::Fill),
                iced_m3::text_field("GiB",app.get(Field::ArcLimit)).id("resource-value-arc").width(104)
                    .on_input(|s|Msg::Edit(Field::ArcLimit,s)).disabled(disabled || draft.automatic)]
                .spacing(12).align_y(Alignment::Center))
            .push(resource_controls::supporting(arc.err().map(|e|e.to_string()).unwrap_or_else(||if draft.automatic {format!("Recommended ceiling: {}. Included in system reserves; VM capacity stays unchanged.",ResourceKind::Memory.format(draft.automatic_maximum.unwrap_or_default() as u128))} else if !draft.automatic_supported {"A reviewed host cache budget is required before editing this policy.".into()} else {"Cache stays within this reserve. Increase memory above for a larger cache.".into()}),draft.arc(app).is_err()));
    }
    if let Some(error) = panel_error(app) {
        body = body.push(copy(error.clone()));
    }
    (
        body.into(),
        row![
            quiet("Cancel", Msg::Close).disabled(app.busy),
            button(if cache_repair {
                "Reapply cache policy"
            } else {
                "Save reserves"
            })
            .on_press(Msg::SaveHostReserves)
            .disabled(
                app.busy
                    || app.snapshot_unavailable
                    || !changed
                    || values.is_err()
                    || draft.arc(app).is_err()
            )
        ]
        .spacing(8)
        .into(),
    )
}

pub(in crate::ui) fn telemetry(s: &Snapshot) -> Element<'static, Msg> {
    let Some(view) = &s.host_reserves else {
        return column![].into();
    };
    let mut memory = column![].spacing(12);
    if let Some(m) = &view.memory {
        let effective = m
            .reviewed_bytes
            .min(m.safe_bytes)
            .saturating_sub(view.active.additional.memory_bytes);
        let held = s
            .users
            .iter()
            .map(|u| u.committed.memory_bytes as u128)
            .sum::<u128>();
        let promised = view.protected_backing.memory_bytes;
        memory = memory
            .push(typography("Physical memory", Type::TitleMedium))
            .push(copy(format!(
                "{:.1} GiB usable · {} GiB reviewed for VMs · {} GiB currently admissible",
                m.physical_bytes as f64 / GIB as f64,
                m.reviewed_bytes / GIB,
                effective / GIB
            )));
        if promised > effective {
            memory=memory.push(copy(format!("Memory promises exceed capacity by {:.1} GiB. Existing grants remain; further memory allocation is blocked.", (promised-effective) as f64/GIB as f64)));
        }
        if held > effective as u128 {
            memory=memory.push(copy("Held VM memory exceeds physical capacity. Stop VMs to release compute, then resize and review grants before restarting."));
        }
        if let Some(error) = &m.error {
            memory = memory.push(copy(error.clone()));
        }
        if s.me.role == Role::Admin && m.safe_bytes != m.reviewed_bytes {
            memory = memory.push(quiet(
                "Review detected RAM change",
                Msg::Open(Panel::ReviewMemoryCapacity),
            ));
        }
    }
    if let Some(op) = &view.memory_operation {
        memory = memory
            .push(copy(
                "A memory capacity update is waiting for host confirmation. Claims stay protected.",
            ))
            .push(
                row![
                    quiet(
                        "Retry capacity update",
                        Msg::RetryMemoryCapacity(op.clone())
                    ),
                    quiet("Cancel unapplied update", Msg::CancelMemoryCapacity(op.id))
                ]
                .spacing(8),
            );
    }
    let Some(arc) = &view.arc else {
        return memory.push(copy("ZFS cache telemetry unavailable.")).into();
    };
    let mode = match &view.active.arc {
        Some(ArcMode::Automatic) => "Automatic (Homecloud)",
        Some(ArcMode::Custom { .. }) => "Custom maximum",
        None => "Configured on host",
    };
    let mut content = column![
        typography("ZFS cache", Type::TitleMedium),
        row![
            vm_stat(
                ResourceKind::Memory.format(arc.size_bytes as u128),
                "Current cache"
            ),
            vm_stat(ResourceKind::Memory.format(arc.maximum_bytes as u128), mode),
            vm_stat(
                ResourceKind::Memory.format(arc.minimum_bytes as u128),
                "Effective cache minimum"
            )
        ]
        .spacing(16),
        copy("The cache allowance is included in system reserves. VM promises remain protected.")
    ]
    .spacing(12);
    if arc.size_bytes > arc.maximum_bytes {
        content=content.push(copy("The new maximum is active. Cache usage can remain above it while buffers await reclamation."));
    }
    memory.push(content).into()
}
