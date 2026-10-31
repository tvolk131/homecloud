use super::*;
use iced::widget::{column, row};
use uuid::Uuid;

pub(super) fn can_edit(app: &App, s: &Snapshot, vm: &Vm) -> bool {
    !app.busy
        && !app.snapshot_unavailable
        && !app.shutdown_blocks_work()
        && lifecycle::unfinished(s, vm).is_none()
        && matches!(
            lifecycle::state(app, s, vm),
            lifecycle::State::Running | lifecycle::State::Stopped
        )
}

pub(super) fn header<'a>(app: &'a App, s: &'a Snapshot, id: Uuid, width: f32) -> Element<'a, Msg> {
    let back = quiet("‹  VMs", Msg::Navigate(Page::Workspaces));
    let Some(vm) = s.vms.iter().find(|v| v.id == id) else {
        return column![back, heading("VM unavailable")].spacing(12).into();
    };
    let status = lifecycle::state(app, s, vm);
    let identity = column![
        typography(&vm.spec.name, Type::HeadlineMedium),
        row![
            row![lifecycle::marker(status), copy(status.label())]
                .spacing(8)
                .align_y(Alignment::Center),
            copy(format!("Ubuntu · {}", s.inventory.hostname))
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    ]
    .spacing(8)
    .width(Length::Fill);
    let actions = row![
        lifecycle::terminal_outlined(app, vm),
        lifecycle::control(app, s, vm)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let top: Element<'a, Msg> = if width < 580.0 {
        column![identity, actions].spacing(16).into()
    } else {
        row![identity, actions]
            .spacing(20)
            .align_y(Alignment::Center)
            .into()
    };
    column![back, top].spacing(16).into()
}

pub(super) fn view<'a>(app: &'a App, s: &'a Snapshot, id: Uuid, width: f32) -> Element<'a, Msg> {
    let Some(vm) = s.vms.iter().find(|v| v.id == id) else {
        return copy("This VM is no longer available.");
    };
    let mut resources = column![
        heading("Resources"),
        typography("Assigned capacity", Type::TitleMedium),
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
            vm_stat(format!("{} GiB", vm.spec.disk_gib), "Disk"),
        ]
        .spacing(16),
    ]
    .spacing(16);
    if vm.desired == Lifecycle::Stopped {
        resources = resources.push(copy(if vm.has_compute() {
            "CPU, RAM and any GPU stay allocated until shutdown and cleanup finish."
        } else {
            "Disk retained. CPU and RAM will be allocated again when you start."
        }));
    }
    resources = resources.push(vm_metrics::view(app, s, vm, width - 40.0));
    let mut body = column![].spacing(20);
    if let Some(recovery) = memory_health::vm_recovery(app, s, vm) {
        body = body.push(recovery);
    }
    if let Some(setup) = setup::card(app, s, vm) {
        body = body.push(setup);
    }
    if let Some(recovery) = lifecycle::recovery(app, s, vm) {
        body = body.push(recovery);
    }
    body = body
        .push(surface(resources, false).padding(20))
        .push(applications::view(app, s, id))
        .push(quiet(
            if app.advanced {
                "Hide advanced"
            } else {
                "Show advanced"
            },
            Msg::ToggleAdvanced,
        ));
    if app.advanced {
        body = body.push(advanced(app, s, vm));
    }
    body.into()
}

fn advanced<'a>(app: &'a App, s: &'a Snapshot, vm: &'a Vm) -> Element<'a, Msg> {
    // Creation can be cancelled without releasing claims before host cleanup.
    let can_delete = !app.busy
        && !app.snapshot_unavailable
        && !app.shutdown_blocks_work()
        && s.operations
            .iter()
            .filter(|op| op.vm_id == vm.id && !matches!(op.phase.as_str(), "succeeded" | "failed"))
            .all(|op| matches!(op.request, Mutation::Create { .. }));
    let mut content = column![].spacing(20);
    let mut details = column![
        heading("Technical details"),
        quiet("View setup logs", Msg::LoadSetupDiagnostics(vm.id))
            .disabled(app.setup_diagnostics_loading),
        quiet(
            if app.identity_details {
                "Hide machine identity"
            } else {
                "Show machine identity"
            },
            Msg::ToggleIdentity
        ),
    ]
    .spacing(8);
    if app.identity_details {
        details = details
            .push(copy(vm.id.to_string()))
            .push(copy("SSH host-key trust is pinned to this identity."));
    }
    if let Some(application) = s.applications.iter().find(|a| a.vm_id == vm.id) {
        details = details.push(applications::details(app, application));
    }
    content = content.push(surface(details, false));
    if vm.observed != "stopped" {
        content = content.push(surface(column![
            heading("Power recovery"),
            copy("Use force stop if the guest cannot shut down normally. Unsaved work and pending writes may be lost."),
            danger("Force stop…", Msg::Open(Panel::ForceStop(vm.id)))
                .disabled(!lifecycle::can_force(app, s, vm)),
        ].spacing(12), false));
    }
    content
        .push(surface(
            column![
                heading("Delete VM"),
                copy("Permanently deletes this VM and its disk. You’ll be asked to confirm."),
                container(
                    button("Delete VM")
                        .variant(Variant::Text)
                        .palette(|t| (Color::TRANSPARENT, t.colors.error))
                        .on_press(Msg::Delete(vm.id))
                        .disabled(!can_delete)
                )
                .id(iced::widget::Id::from(format!("vm-delete-{}", vm.id))),
            ]
            .spacing(8),
            false,
        ))
        .into()
}
