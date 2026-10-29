use super::*;
use iced::widget::{column, row};

fn quota(
    label: &'static str,
    allocated: u64,
    limit: u64,
    unit: &'static str,
) -> Element<'static, Msg> {
    let over = allocated > limit;
    let unit_for = |count| {
        if unit == "cores" && count == 1 {
            "core"
        } else {
            unit
        }
    };
    let fraction = if limit == 0 {
        if allocated == 0 { 0.0 } else { 1.0 }
    } else {
        (allocated as f32 / limit as f32).min(1.0)
    };
    column![
        copy(label),
        typography(
            format!("{allocated} / {limit} {}", unit_for(limit)),
            Type::TitleMedium
        ),
        iced_m3::linear_progress(fraction),
        typography(
            if over {
                let difference = allocated - limit;
                format!("{difference} {} over quota", unit_for(difference))
            } else {
                let remaining = limit - allocated;
                format!("{remaining} {} available", unit_for(remaining))
            },
            Type::BodySmall
        )
        .style(move |theme: &Theme| text::Style {
            color: Some(if over {
                theme.colors.error
            } else {
                theme.colors.on_surface_variant
            })
        }),
    ]
    .spacing(6)
    .width(Length::Fill)
    .into()
}

pub(super) fn view<'a>(app: &'a App, snapshot: &'a Snapshot, width: f32) -> Element<'a, Msg> {
    let user = &snapshot.me;
    let capacity = surface(
        column![
            row![
                typography("Your capacity", Type::TitleMedium),
                horizontal_space(),
                copy("Allocated / quota")
            ]
            .align_y(Alignment::Center),
            row![
                quota(
                    "Memory",
                    user.committed.memory_bytes / GIB,
                    user.grant.memory_bytes / GIB,
                    "GiB"
                ),
                quota(
                    "Dedicated CPU",
                    user.committed.cpu_cores,
                    user.grant.cpu_cores,
                    "cores"
                ),
                quota(
                    "Disk backing",
                    user.committed.disk_bytes / GIB,
                    user.grant.disk_bytes / GIB,
                    "GiB"
                )
            ]
            .spacing(24),
        ]
        .spacing(12),
        false,
    )
    .padding(20);
    let mut body = column![
        capacity,
        row![
            heading("VMs"),
            horizontal_space(),
            copy(format!("{} total", snapshot.vms.len()))
        ]
        .align_y(Alignment::Center),
    ]
    .spacing(20);
    if snapshot.vms.is_empty() {
        return body
            .push(surface(
                column![
                    heading("A VM of your own"),
                    copy("Your projects stay on the VM when your Mac disconnects."),
                    copy("Choose New VM to create your first VM."),
                ]
                .spacing(16),
                false,
            ))
            .into();
    }
    let columns = if width >= 840.0 { 2 } else { 1 };
    for chunk in snapshot.vms.chunks(columns) {
        let mut line = row![].spacing(16);
        for vm in chunk {
            line = line.push(card(app, snapshot, vm));
        }
        if chunk.len() < columns {
            line = line.push(horizontal_space().width(Length::Fill));
        }
        body = body.push(line);
    }
    body.push(copy(
        "Stopped VMs keep their disks. CPU, RAM and GPUs are released after cleanup.",
    ))
    .into()
}

fn app_summary(snapshot: &Snapshot, vm: &Vm) -> &'static str {
    if vm.spec.managed_host() {
        return "Ubuntu · Homecloud host";
    }
    let Some(application) = snapshot
        .applications
        .iter()
        .find(|a| a.vm_id == vm.id && a.kind == ApplicationKind::T3)
    else {
        return "Ubuntu · standard VM";
    };
    let observation = &application.observation;
    if snapshot.application_operations.iter().any(|op| {
        op.application_id == application.id
            && op.kind == ApplicationOperationKind::Install
            && op.phase.active()
    }) {
        "Ubuntu · installing T3…"
    } else if observation.installed_version.is_none() {
        "Ubuntu · T3 setup needed"
    } else if vm.desired != Lifecycle::Running || vm.observed != "running" {
        "Ubuntu · T3 installed"
    } else if observation.setup == "ready"
        && observation.service == "running"
        && !observation.components.is_empty()
        && observation.components.iter().all(|c| c.ready)
        && observation
            .components
            .iter()
            .any(|c| c.account == "signed_in")
    {
        "Ubuntu · ready in T3"
    } else {
        "Ubuntu · T3 setup needed"
    }
}

fn card<'a>(app: &'a App, snapshot: &'a Snapshot, vm: &'a Vm) -> Element<'a, Msg> {
    let status = lifecycle::state(app, snapshot, vm);
    let mut identity = column![
        row![
            lifecycle::marker(status),
            typography(&vm.spec.name, Type::TitleLarge)
        ]
        .spacing(12)
        .align_y(Alignment::Center),
        copy(app_summary(snapshot, vm)),
    ]
    .spacing(4)
    .width(Length::Fill);
    if vm.owner != snapshot.me.id
        && let Some(owner) = snapshot.users.iter().find(|u| u.id == vm.owner)
    {
        identity = identity.push(copy(format!("Owner · {}", owner.name)));
    }
    let name = iced_m3::Button::new(identity)
        .variant(Variant::Text)
        .width(Length::Fill)
        .padding([8, 12])
        .radius(12.0)
        .on_press(Msg::OpenWorkspace(vm.id));
    let identity = container(iced_m3::tooltip(name, status.label()))
        .id(format!("vm-name-{}", vm.id))
        .width(Length::Fill);
    let mut content = column![
        row![identity, lifecycle::control(app, snapshot, vm)]
            .spacing(12)
            .align_y(Alignment::Center),
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
        .spacing(12),
    ]
    .spacing(16);
    if matches!(
        status,
        lifecycle::State::Attention | lifecycle::State::Cleanup | lifecycle::State::Unavailable
    ) {
        content = content.push(copy(status.label()));
    }
    if status == lifecycle::State::Attention {
        if let Some(error) = &vm.error {
            content = content.push(copy(error.strip_prefix("pending: ").unwrap_or(error)));
        }
        content = content.push(quiet("Review issue", Msg::OpenWorkspace(vm.id)));
    }
    if let Some(health) = snapshot.memory_health.as_ref().filter(|h| h.waiting(vm)) {
        content = content.push(copy(health.condition.title()));
    }
    if !vm.gpus.is_empty() {
        content = content.push(badge(
            if vm.gpus.len() == 1 {
                "GPU allocated".into()
            } else {
                format!("{} GPUs allocated", vm.gpus.len())
            },
            true,
        ));
    } else if !vm.preferred_gpus.is_empty() {
        content = content.push(copy(if vm.preferred_gpus.len() == 1 {
            "GPU requested on next start".into()
        } else {
            format!("{} GPUs requested on next start", vm.preferred_gpus.len())
        }));
    }
    if vm.observed == "running" && vm.desired == Lifecycle::Running && !status.busy() {
        content = content.push(lifecycle::terminal(app, vm));
    }
    surface(content, false).padding(20).into()
}
