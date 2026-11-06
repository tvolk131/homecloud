use super::*;
use crate::virtualization::{Cockpit, Confirmation, Event};
use iced::widget::{column, row};

pub(super) fn problem(value: &str) -> Element<'_, Msg> {
    container(typography(value, Type::BodyMedium))
        .padding(16)
        .width(Length::Fill)
        .style(|theme: &Theme| container::Style {
            background: Some(theme.colors.error_container.into()),
            text_color: Some(theme.colors.on_error_container),
            border: Border {
                radius: 16.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

pub(super) fn action_label(action: VirtualizationPowerAction) -> &'static str {
    match action {
        VirtualizationPowerAction::Start => "Start",
        VirtualizationPowerAction::Shutdown => "Shut down",
        VirtualizationPowerAction::ForceStop => "Force stop",
    }
}

pub(super) fn controls<'a>(cockpit: &'a Cockpit, guest: &VirtualizationGuest) -> Element<'a, Msg> {
    if guest.template || guest.kind != VirtualizationGuestKind::VirtualMachine {
        return copy("Power controls are available only for non-template QEMU VMs.");
    }
    let identity = VirtualizationGuestIdentity {
        node: guest.node.clone(),
        vmid: guest.vmid,
        kind: guest.kind,
    };
    let action = match guest.status.as_str() {
        "stopped" => VirtualizationPowerAction::Start,
        "running" => VirtualizationPowerAction::Shutdown,
        _ => return copy("No power action is available for this reported state."),
    };
    let reason = cockpit.power_block_reason(&identity, action, crate::unix_now(), false);
    let mut body = column![
        heading("VM power"),
        copy("The server rechecks live inventory and guest state before accepting an action."),
        secondary(
            action_label(action),
            Msg::Cockpit(Event::ReviewPower(identity.clone(), action))
        )
        .disabled(reason.is_some()),
    ]
    .spacing(12);
    if action == VirtualizationPowerAction::Shutdown {
        body = body.push(copy("Shut down asks the guest to stop gracefully. Force stop abruptly cuts power and risks data loss."))
            .push(danger("Force stop…", Msg::Cockpit(Event::ReviewPower(identity, VirtualizationPowerAction::ForceStop))).disabled(reason.is_some()));
    }
    if let Some(reason) = reason {
        body = body.push(copy(reason));
    }
    body.into()
}

pub(super) fn confirmation(cockpit: &Cockpit) -> Option<Element<'_, Msg>> {
    let review = cockpit.confirmation.as_ref()?;
    let token = review.token();
    let (title, description, label, blocked) = match review {
        Confirmation::Power(request) | Confirmation::Retry { request, .. } => {
            let recovering = matches!(review, Confirmation::Retry { .. });
            let reason = cockpit.power_block_reason(
                &request.guest,
                request.action,
                crate::unix_now(),
                recovering,
            );
            let description = match request.action {
                VirtualizationPowerAction::Start => {
                    "Start powers on this VM. It does not create a VM, install T3, or open SSH."
                }
                VirtualizationPowerAction::Shutdown => {
                    "Ask this VM to shut down gracefully. Applications may disconnect; completion is tracked by the Proxmox task."
                }
                VirtualizationPowerAction::ForceStop => {
                    "Abruptly cuts power. Unsaved data may be lost and guest filesystems may be damaged. Prefer graceful Shut down."
                }
            };
            (
                format!(
                    "{} VM {} on {}?",
                    action_label(request.action),
                    request.guest.vmid,
                    request.guest.node
                ),
                description,
                match request.action {
                    VirtualizationPowerAction::Start => "Confirm start",
                    VirtualizationPowerAction::Shutdown => "Confirm shut down",
                    VirtualizationPowerAction::ForceStop => "Confirm force stop",
                },
                reason,
            )
        }
        Confirmation::Acknowledge { operation_id, .. } => (
            "Release Unknown safety hold?".into(),
            "Independently inspect this VM and its tasks in Proxmox first. Acknowledgement only releases the safety hold; it does not retry the action or verify success.",
            "I inspected Proxmox; release hold",
            (!cockpit.can_acknowledge(*operation_id, crate::unix_now()))
                .then_some("A fresh operations list and no other submission are required."),
        ),
    };
    let mut body = column![heading(title), copy(description)].spacing(12);
    match review {
        Confirmation::Retry { request, .. } => {
            body = body.push(copy(format!("Same request key: {}. This checks an already accepted request or submits the original action once if it was not accepted. No new key is generated.", request.key)));
        }
        Confirmation::Acknowledge { operation_id, .. } => {
            if let Some(operation) = cockpit
                .operations
                .iter()
                .find(|operation| operation.id == *operation_id)
            {
                body = body.push(copy(format!(
                    "{} · VM {} · node {} · operation {}",
                    action_label(operation.action),
                    operation.guest.vmid,
                    operation.guest.node,
                    operation.id
                )));
            }
        }
        Confirmation::Power(_) => {}
    }
    if let Some(reason) = blocked {
        body = body.push(copy(reason));
    }
    let confirm = if matches!(review, Confirmation::Power(request) | Confirmation::Retry { request, .. } if request.action == VirtualizationPowerAction::ForceStop)
        || matches!(review, Confirmation::Acknowledge { .. })
    {
        danger(label, Msg::Cockpit(Event::Confirm(token)))
    } else {
        secondary(label, Msg::Cockpit(Event::Confirm(token)))
    };
    body = body.push(
        row![
            quiet("Cancel", Msg::Cockpit(Event::Cancel(token))),
            confirm.disabled(blocked.is_some()),
        ]
        .spacing(12)
        .wrap(),
    );
    Some(surface(body, true).into())
}

pub(super) fn recovery(cockpit: &Cockpit) -> Option<Element<'_, Msg>> {
    let pending = cockpit.pending.as_ref()?;
    let request = &pending.request;
    let mut body = column![
        heading(if pending.in_flight {
            "Submitting power request…"
        } else {
            "Power request outcome not confirmed"
        }),
        copy(format!(
            "{} · VM {} · node {}",
            action_label(request.action),
            request.guest.vmid,
            request.guest.node
        )),
        copy(format!("Original request key: {}", request.key)),
    ]
    .spacing(12);
    if let Some(error) = &pending.error {
        body = body.push(copy(error));
    }
    if !pending.in_flight {
        body = body.push(copy("The request may already be accepted. No automatic retry will occur. Other power actions stay paused until this request is resolved."));
        if pending.checked_missing {
            body = body.push(copy("The original key was not found in the latest operations list (up to 100 records). That does not prove rejection. An explicit same-key retry uses server deduplication and rechecks live state."));
        }
        let reason =
            cockpit.power_block_reason(&request.guest, request.action, crate::unix_now(), true);
        body = body.push(
            row![
                secondary(
                    "Check original request",
                    Msg::Cockpit(Event::CheckRequest(request.key))
                )
                .disabled(cockpit.operations_loading),
                secondary(
                    "Review same-key retry",
                    Msg::Cockpit(Event::RetryRequest(request.key))
                )
                .disabled(reason.is_some()),
            ]
            .spacing(12)
            .wrap(),
        );
        if let Some(reason) = reason {
            body = body.push(copy(reason));
        }
    }
    Some(surface(body, true).into())
}

pub(super) fn history(cockpit: &Cockpit) -> Element<'_, Msg> {
    let mut body = column![
        heading("Power operations"),
        copy("Durable server history · up to 100 recent operations, active first. Progress is not inferred from guest metrics. No power action is replayed when this app starts."),
    ].spacing(12);
    let mut count = 0;
    for operation in &cockpit.operations {
        if cockpit.selected_guest.as_ref().is_some_and(|guest| {
            guest.node != operation.guest.node || guest.vmid != operation.guest.vmid
        }) {
            continue;
        }
        count += 1;
        let phase = match operation.phase {
            VirtualizationOperationPhase::Submitting => "Submitting to Proxmox",
            VirtualizationOperationPhase::Running => "Running · waiting for Proxmox task",
            VirtualizationOperationPhase::Succeeded => "Succeeded",
            VirtualizationOperationPhase::Failed => "Failed",
            VirtualizationOperationPhase::Unknown => "Unknown · safety hold",
            VirtualizationOperationPhase::Acknowledged => {
                "Acknowledged · hold released, not retried"
            }
        };
        let mut details = column![
            heading(format!(
                "{} · VM {} · {}",
                action_label(operation.action),
                operation.guest.vmid,
                operation.guest.node
            )),
            badge(
                phase,
                operation.phase == VirtualizationOperationPhase::Succeeded
            ),
            copy(format!("Operation: {}", operation.id)),
            copy(format!(
                "Requested by: {} · Created: {} · Updated: {}",
                operation.requested_by, operation.created_at, operation.updated_at
            )),
            copy(format!(
                "Proxmox task: {}",
                operation.task_id.as_deref().unwrap_or("Not recorded")
            )),
        ]
        .spacing(10);
        if let Some(error) = &operation.error {
            details = details.push(problem(error));
        }
        if operation.phase == VirtualizationOperationPhase::Unknown {
            details = details.push(copy("The action may have reached Proxmox. It will never be automatically retried. Inspect Proxmox independently before acknowledging this hold."))
                .push(danger("Review safety hold…", Msg::Cockpit(Event::ReviewAcknowledgement(operation.id)))
                    .disabled(!cockpit.can_acknowledge(operation.id, crate::unix_now())));
        }
        if cockpit.acknowledging == Some(operation.id) {
            details = details.push(copy("Acknowledging safety hold…"));
        }
        body = body.push(surface(details, false));
    }
    if count == 0 {
        body = body.push(copy(if cockpit.operations_stale(crate::unix_now()) {
            "Operation history is not yet current. Power controls remain paused."
        } else {
            "No power operations returned for this view."
        }));
    }
    body.into()
}
