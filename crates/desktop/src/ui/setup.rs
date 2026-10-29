use super::*;
use iced::widget::{column, row};
use uuid::Uuid;

pub(super) fn card<'a>(app: &App, s: &'a Snapshot, vm: &'a Vm) -> Option<Element<'a, Msg>> {
    let op = s.operations.iter().find(|op| {
        op.vm_id == vm.id
            && !matches!(op.phase.as_str(), "succeeded" | "failed")
            && !op.request.stops_vm()
    })?;
    let review = op.phase == "needs_review";
    let mut body =
        column![
            heading(if review {
                "VM setup needs attention"
            } else {
                "Getting your VM ready"
            }),
            copy(op.progress.as_ref().map(|p| p.stage.label()).unwrap_or(
                if op.phase == "pending" {
                    "Request queued"
                } else {
                    "Preparing VM resources"
                }
            )),
        ]
        .spacing(12);
    if let Some(error) = op
        .error
        .as_ref()
        .filter(|error| op.progress.is_none() || !error.starts_with("pending:"))
    {
        body = body.push(copy(
            error
                .strip_prefix("pending: ")
                .or_else(|| error.strip_prefix("conflict: "))
                .unwrap_or(error),
        ));
    }
    body = body.push(copy("CPU, memory and any GPUs stay allocated until shutdown and cleanup finish. Disk stays allocated until deletion."));
    let mut actions = row![
        secondary("View setup details", Msg::LoadSetupDiagnostics(vm.id))
            .disabled(app.setup_diagnostics_loading)
    ]
    .spacing(8);
    if review {
        body = body.push(copy("Review the failure before retrying. Retry keeps the disk and checks the existing setup; failed guest installation steps need repair inside the VM."));
        if s.me.role == Role::Admin {
            actions = actions.push(
                quiet("Retry after repair", Msg::Retry(op.id))
                    .disabled(app.busy || app.snapshot_unavailable),
            );
        } else {
            body = body.push(copy(
                "Ask an administrator to review the failure before retrying.",
            ));
        }
        if matches!(op.request, Mutation::Create { .. }) {
            actions = actions.push(
                button("Delete incomplete VM…")
                    .variant(Variant::Text)
                    .palette(|t| (Color::TRANSPARENT, t.colors.error))
                    .on_press(Msg::Delete(vm.id))
                    .disabled(app.busy || app.snapshot_unavailable),
            );
        }
    }
    Some(surface(body.push(actions.wrap()), false).into())
}

pub(super) fn details<'a>(app: &'a App, id: Uuid) -> Element<'a, Msg> {
    column![
        heading("VM setup details"),
        scrollable(contents(app, id))
            .width(Length::Fill)
            .height(360)
            .id("setup-log-output")
    ]
    .spacing(16)
    .into()
}
fn contents<'a>(app: &'a App, id: Uuid) -> Element<'a, Msg> {
    let mut body = column![].spacing(16).width(Length::Fill);
    if app.setup_diagnostics_loading {
        return body
            .push(copy("Reading setup status and guest logs…"))
            .into();
    }
    match app
        .setup_diagnostics
        .as_ref()
        .filter(|(vm, _)| *vm == id)
        .map(|(_, report)| report)
    {
        Some(Ok(report)) => {
            body = body.push(copy(format!("Incus state: {}", report.power_state)));
            if let Some(progress) = &report.progress {
                body = body.push(copy(progress.stage.label()));
            }
            for check in &report.checks {
                body = body.push(copy(check));
            }
            body = body.push(copy("Recent output only. Logs are fetched on request and are not saved in Homecloud’s database."));
            let mut logs = column![].spacing(20).width(Length::Fill);
            for log in &report.logs {
                logs = logs.push(
                    column![
                        typography(&log.name, Type::TitleSmall),
                        text(&log.text)
                            .font(iced::Font::MONOSPACE)
                            .size(12)
                            .width(Length::Fill)
                    ]
                    .spacing(8),
                );
            }
            if !report.logs.is_empty() {
                body = body.push(logs);
            }
        }
        Some(Err(error)) => {
            body = body
                .push(copy(format!("Could not read setup details: {error}")))
                .push(copy(
                    "Your allocations are unchanged. Refresh to try again.",
                ));
        }
        None => {
            body = body.push(copy("Choose Refresh to read setup details."));
        }
    }
    body.into()
}
