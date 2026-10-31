use super::*;
use iced::widget::{column, row};

fn resource(kind: ResourceKind, user: &User) -> Element<'static, Msg> {
    let grant = kind.value(user.grant);
    let held = kind.value(user.committed);
    let fraction = if grant == 0 {
        if held == 0 { 0.0 } else { 1.0 }
    } else {
        (held as f32 / grant as f32).min(1.0)
    };
    column![
        copy(kind.label()),
        typography(
            format!("{} / {}", kind.format(held), kind.format(grant)),
            Type::TitleMedium
        ),
        iced_m3::linear_progress(fraction),
        copy(if held > grant {
            format!("{} above grant", kind.format(held - grant))
        } else {
            format!("{} unused", kind.format(grant - held))
        }),
    ]
    .spacing(6)
    .width(Length::Fill)
    .into()
}

pub(super) fn view<'a>(app: &'a App, s: &'a Snapshot) -> Element<'a, Msg> {
    let admin = s.me.role == Role::Admin;
    let model = Allocation::new(s, ResourceKind::Memory);
    let mut body = column![
        typography("People on this host", Type::TitleLarge),
        copy("Allocated / grant"),
    ]
    .spacing(16);
    for owner in &model.owners {
        let user = owner.user;
        let mut header = row![
            swatch(Some(user.id), false),
            typography(&user.name, Type::TitleMedium)
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if !user.active {
            header = header.push(copy("Account disabled"));
        }
        header = header.push(horizontal_space());
        if admin {
            header = header.push(
                container(
                    quiet("Edit grant", Msg::Select(user.id))
                        .disabled(app.busy || app.snapshot_unavailable),
                )
                .id(iced::widget::Id::from(format!("host-grant-{}", user.id))),
            );
        }
        let mut content = column![
            header,
            row![
                resource(ResourceKind::Memory, user),
                resource(ResourceKind::Cpu, user),
                resource(ResourceKind::Storage, user),
            ]
            .spacing(24)
        ]
        .spacing(16);
        if ResourceKind::ALL
            .into_iter()
            .any(|kind| kind.value(user.committed) > kind.value(user.grant))
        {
            content = content.push(copy(
                "Existing allocations are retained. Growth above a resource’s grant is paused.",
            ));
        }
        let vms: Vec<_> = s
            .vms
            .iter()
            .filter(|vm| vm.owner == user.id && vm.observed != "deleted")
            .collect();
        if !vms.is_empty() {
            let expanded = app.host_owner == Some(user.id);
            content = content.push(
                button(format!(
                    "{} VMs ({})",
                    if expanded { "Hide" } else { "Show" },
                    vms.len()
                ))
                .variant(Variant::Text)
                .on_press(Msg::HighlightOwner(if expanded {
                    None
                } else {
                    Some(user.id)
                })),
            );
            if expanded {
                for vm in vms {
                    content = content.push(
                        row![
                            column![
                                typography(&vm.spec.name, Type::BodyMedium),
                                copy(format!(
                                    "{} memory · {} CPU · {} storage · {}",
                                    ResourceKind::Memory.format(vm.resources.memory_bytes as u128),
                                    ResourceKind::Cpu.format(vm.resources.cpu_cores as u128),
                                    ResourceKind::Storage.format(vm.resources.disk_bytes as u128),
                                    lifecycle::state(app, s, vm).label()
                                ))
                            ]
                            .spacing(4)
                            .width(Length::Fill),
                            quiet("View VM", Msg::OpenWorkspace(vm.id))
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    );
                }
            }
        }
        body = body.push(surface(content, false));
    }
    body = body.push(copy("Protected backing is the larger of each person’s grant and held VM allocations, for each resource. Lowering a grant does not release existing backing."));
    body.into()
}
