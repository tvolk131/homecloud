use super::*;
use iced::widget::{column, row};

pub(super) fn banner<'a>(app: &'a App, s: &'a Snapshot) -> Option<Element<'a, Msg>> {
    let op = s
        .host_shutdown
        .as_ref()
        .filter(|op| op.phase.blocks_work())?;
    let title = if app.snapshot_unavailable {
        "Host unreachable"
    } else {
        match op.phase {
            HostShutdownPhase::Draining => "Shutting down VMs…",
            HostShutdownPhase::PoweroffPending | HostShutdownPhase::PoweroffRequested => {
                "Powering off host…"
            }
            _ => "Host shutdown needs review",
        }
    };
    let detail = if app.snapshot_unavailable {
        "The host stopped responding after a shutdown request. Its final power state cannot be verified.".into()
    } else if op.phase == HostShutdownPhase::Draining {
        format!(
            "{} of {} VMs checked. VM changes are paused.",
            op.vm_ids.len().saturating_sub(op.remaining.len()),
            op.vm_ids.len()
        )
    } else if op.phase == HostShutdownPhase::PoweroffRequested {
        "Power-off was requested. Waiting for the host to disconnect; completion cannot be verified remotely.".into()
    } else {
        "VM changes are paused. VM settings, disks and allocations are kept.".into()
    };
    let mut content = column![heading(title), copy(detail)].spacing(8);
    if app.page == Page::Host && !app.snapshot_unavailable {
        if let Some(error) = &op.error {
            content = content.push(copy(error));
        }
        if op.phase == HostShutdownPhase::NeedsReview && s.me.role == Role::Admin {
            let mut actions =
                row![secondary("Retry shutdown", Msg::RetryHostShutdown(op.id)).disabled(app.busy)]
                    .spacing(8);
            if !op.poweroff_attempted {
                actions = actions.push(
                    quiet(
                        "Cancel shutdown and resume VMs",
                        Msg::CancelHostShutdown(op.id),
                    )
                    .disabled(app.busy),
                );
            }
            content = content.push(actions.wrap());
        }
    } else if app.page != Page::Host {
        content = content.push(quiet("View host", Msg::Navigate(Page::Host)));
    }
    Some(surface(content, false).padding(16).into())
}
