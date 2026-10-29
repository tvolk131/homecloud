use super::*;
use iced::widget::{column, row};

/// Unmanaged capacity is not an enforceable reservation: it can also cover
/// workloads outside Homecloud. ZFS's free-space floor is reported separately.
pub(super) fn view(app: &App, s: &Snapshot) -> Element<'static, Msg> {
    let i = &s.inventory;
    let memory = if i.memory_bytes > 0 && i.memory_bytes >= s.capacity.memory_bytes {
        ResourceKind::Memory.format((i.memory_bytes - s.capacity.memory_bytes) as u128)
    } else {
        "Not reported".into()
    };
    let cpu = if i.physical_cores > 0 && i.physical_cores as u64 >= s.capacity.cpu_cores {
        ResourceKind::Cpu.format(i.physical_cores as u128 - s.capacity.cpu_cores as u128)
    } else {
        "Not reported".into()
    };
    let disk = i
        .minimum_pool_available_bytes
        .map(|v| ResourceKind::Storage.format(v as u128))
        .unwrap_or_else(|| "Not reported".into());
    let total = s
        .host_reserves
        .as_ref()
        .and_then(|v| v.minimum_reserves.checked_add(v.active.additional).ok());
    let explicit = s
        .host_reserves
        .as_ref()
        .is_some_and(|v| v.minimum_reserves != Resources::default());
    let memory = if explicit {
        ResourceKind::Memory.format(total.unwrap_or_default().memory_bytes as u128)
    } else {
        memory
    };
    let cpu = if explicit {
        ResourceKind::Cpu.format(total.unwrap_or_default().cpu_cores as u128)
    } else {
        cpu
    };
    let mut header = row![
        heading(if explicit {
            "System reserves"
        } else {
            "Host headroom"
        }),
        horizontal_space()
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    if let Some(view) = &s.host_reserves {
        header = header.push(
            quiet("Edit reserves", Msg::Open(Panel::HostReserves)).disabled(
                app.busy
                    || app.snapshot_unavailable
                    || view
                        .operation
                        .as_ref()
                        .is_some_and(|op| !op.phase.terminal()),
            ),
        );
    } else {
        header = header.push(copy("Configured on host"));
    }
    let mut body=column![header,
        row![
            vm_stat(memory, if explicit {"System memory reserve"} else {"Memory outside VM budget"}),
            vm_stat(cpu, if explicit {"System CPU reserve"} else {"Cores outside VM budget"}),
            vm_stat(disk, "Pool free-space floor"),
        ].spacing(16),
        copy(if explicit {"System reserves protect operating headroom. The pool floor is a minimum free-space policy, not a ZFS reservation."} else {"Outside-budget memory and cores cover host services, safety headroom and any unmanaged workloads. The pool floor is a minimum free-space policy, not a ZFS reservation."}),
    ].spacing(14);
    if let Some(view) = &s.host_reserves {
        if let Some(policy) = view.memory_policy {
            let allowance = policy
                .allowance(total.unwrap_or_default().memory_bytes)
                .unwrap_or(0);
            body = body.push(copy(format!(
                "Memory includes {} for services and VM overhead, with up to {} for ZFS cache.",
                ResourceKind::Memory.format(policy.operating_floor_bytes as u128),
                ResourceKind::Memory.format(allowance as u128)
            )));
        }
        let extra = if explicit {
            Resources::default()
        } else {
            view.active.additional
        };
        if extra != Resources::default() {
            body = body.push(copy(format!(
                "Additional system reserves: {} memory · {} CPU · {} disk.",
                ResourceKind::Memory.format(extra.memory_bytes as u128),
                ResourceKind::Cpu.format(extra.cpu_cores as u128),
                ResourceKind::Storage.format(extra.disk_bytes as u128)
            )));
        }
        if let Some(op) = view.operation.as_ref().filter(|op| !op.phase.terminal()) {
            if op.phase == ReservePhase::NeedsReview {
                body = body.push(copy(
                    op.error
                        .clone()
                        .unwrap_or_else(|| "Host reserve change needs review.".into()),
                ));
                let mut actions = row![
                    quiet("Retry reserve change", Msg::RetryHostReserves(op.id))
                        .disabled(app.busy || app.snapshot_unavailable)
                ]
                .spacing(8);
                if view.can_discard {
                    actions = actions.push(
                        quiet("Discard unapplied change", Msg::DiscardHostReserves(op.id))
                            .disabled(app.busy || app.snapshot_unavailable),
                    );
                }
                body = body.push(actions);
            } else {
                if let Some(message) = op.error.as_deref().and_then(|e| e.strip_prefix("pending:"))
                {
                    body = body.push(copy(message.trim().to_owned()));
                }
                body=body.push(row![iced_m3::circular_progress(0.0).indeterminate(true).paused(app.reduced_motion).size(20.0),
                    copy("Updating host reserves… Capacity stays protected until the host confirms.")]
                    .spacing(10).align_y(Alignment::Center));
            }
        }
    }
    if s.host_reserves.as_ref().is_some_and(|v| v.arc.is_some()) {
        body = body.push(crate::ui::reserves::telemetry(s));
    }
    surface(body, false).into()
}
