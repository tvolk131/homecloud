use super::*;
use iced::widget::{column, row};
use uuid::Uuid;

pub(super) fn form<'a>(app: &'a App, id: Uuid) -> (Element<'a, Msg>, Element<'a, Msg>) {
    let Some(s) = app.snapshot.as_ref() else {
        return (copy("VM unavailable"), quiet("Cancel", Msg::Close).into());
    };
    let Some(vm) = s.vms.iter().find(|v| v.id == id) else {
        return (copy("VM unavailable"), quiet("Cancel", Msg::Close).into());
    };
    let selected = app.edit_gpu.target_gpus(vm);
    let initial = GpuEdit::Keep.target_gpus(vm);
    let draft = app.workspace_edit().and_then(|d| {
        if let Some(bounds) = crate::resources::FormBounds::displayed(app) {
            bounds.validate(app).map_err(anyhow::Error::msg)?;
        }
        anyhow::ensure!(
            d.disk_gib >= vm.spec.disk_gib,
            "Disk capacity can only grow."
        );
        Ok(d)
    });
    let restart = draft.as_ref().is_ok_and(|d| d.requires_restart(vm));
    let changed = draft.as_ref().is_ok_and(|d| d.changed(vm));
    // A fixed helper area keeps the form stationary as validation changes.
    let helper = draft
        .as_ref()
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| {
            if vm.desired == Lifecycle::Stopped {
                "Disk can only grow. CPU and RAM availability is checked on Start.".into()
            } else {
                "Disk can only grow. Availability is checked again when saving.".into()
            }
        });
    let mut content = column![
        app.input("VM name", Field::Name),
        heading("Resources"),
        resource_controls::controls(app),
        resource_controls::supporting(helper, draft.is_err()),
        row![
            typography("GPUs", Type::TitleMedium),
            horizontal_space(),
            copy(format!("{} selected", selected.len()))
        ]
        .align_y(Alignment::Center),
    ]
    .spacing(12);
    let permitted = s
        .users
        .iter()
        .find(|u| u.id == vm.owner)
        .map(|u| u.gpu_allowed)
        .unwrap_or(s.me.gpu_allowed);
    if !permitted {
        content = content.push(copy(
            "GPU access is disabled. Existing selections can be kept or removed.",
        ));
    }
    let devices = canonical_gpu_set(
        s.inventory
            .gpus
            .iter()
            .map(|g| g.pci.clone())
            .chain(initial.iter().cloned())
            .collect(),
    );
    if devices.is_empty() {
        content = content.push(copy("No GPUs detected on this host."));
    }
    for pci in devices {
        let gpu = s
            .inventory
            .gpus
            .iter()
            .find(|g| g.pci.eq_ignore_ascii_case(&pci));
        let other = s.vms.iter().any(|v| v.id != id && v.claims_gpu(&pci));
        let available =
            vm.claims_gpu(&pci) || gpu.is_some_and(|g| g.readiness.state == "ready" && !other);
        let existing = initial.contains(&pci);
        let checked = selected.contains(&pci);
        let label = format!(
            "{} · {pci}{}",
            gpu.map(|g| gpu_name(&g.model))
                .unwrap_or_else(|| "GPU not detected".into()),
            if available { "" } else { " · unavailable now" }
        );
        let may_select = existing
            || (permitted
                && if vm.desired == Lifecycle::Stopped {
                    gpu.is_some_and(|g| matches!(g.readiness.state.as_str(), "ready" | "assigned"))
                } else {
                    available
                });
        content = content.push(
            iced_m3::checkbox(checked)
                .label(label)
                .on_toggle(move |enabled| Msg::ToggleEditGpu(pci.clone(), enabled))
                .disabled(app.busy || (!checked && !may_select)),
        );
    }
    content=content.push(copy(if vm.desired==Lifecycle::Stopped {
        "Select GPUs for the next start. Leave all unchecked for none; availability is checked when starting."
    }else{"Select any GPUs to attach. Leave all unchecked for none."}));
    // A boot without GPUs must not silently erase the next-start preference.
    if vm.desired == Lifecycle::Running && vm.gpus.is_empty() && !vm.preferred_gpus.is_empty() {
        content = content
            .push(copy(format!(
                "{} GPU preference(s) remembered for the next start.",
                vm.preferred_gpus.len()
            )))
            .push(
                quiet(
                    if matches!(app.edit_gpu, GpuEdit::Keep) {
                        "Forget remembered GPUs"
                    } else {
                        "Keep remembered GPUs"
                    },
                    Msg::EditGpu(if matches!(app.edit_gpu, GpuEdit::Keep) {
                        GpuEdit::SetMany { pcis: vec![] }
                    } else {
                        GpuEdit::Keep
                    }),
                )
                .disabled(app.busy || !selected.is_empty()),
            );
    }
    let actions = row![
        quiet("Cancel", Msg::Close).disabled(app.busy),
        button(if app.busy {
            "Saving…"
        } else if restart {
            "Save & restart…"
        } else {
            "Save changes"
        })
        .on_press(Msg::SaveWorkspace(id))
        .disabled(app.busy || app.snapshot_unavailable || !changed || draft.is_err())
    ]
    .spacing(8)
    .wrap();
    let mut form = column![heading("Edit VM")].spacing(12);
    if let Some(error) = panel_error(app) {
        form = form.push(message(error, true));
    }
    (form.push(content).into(), actions.into())
}

pub(super) fn confirmation<'a>(
    app: &'a App,
    id: Uuid,
    changes: &'a WorkspaceEdit,
) -> (Element<'a, Msg>, Element<'a, Msg>) {
    let vm = app
        .snapshot
        .as_ref()
        .and_then(|s| s.vms.iter().find(|v| v.id == id));
    let name = vm.map(|v| v.spec.name.as_str()).unwrap_or("this VM");
    let mut content=column![
        copy("The VM will shut down and start again. Running tasks may be interrupted, and terminal and T3 connections will disconnect until it is back online. Save your work before continuing."),
        copy(format!("{} · {} GiB RAM · {} core{} · {} GiB disk",changes.name,changes.memory_gib,changes.cores,if changes.cores==1{""}else{"s"},changes.disk_gib)),
        copy("Homecloud checks your resource quota and all selected GPU allocations before shutting down the VM."),
    ].spacing(16);
    if !matches!(changes.gpu, GpuEdit::Keep)
        && let Some(vm) = vm
    {
        let targets = changes.target_gpus(vm);
        if targets.is_empty() {
            content = content.push(copy(
                "All GPUs will be detached and remembered preferences cleared.",
            ));
        } else {
            for pci in targets {
                let model = app
                    .snapshot
                    .as_ref()
                    .and_then(|s| {
                        s.inventory
                            .gpus
                            .iter()
                            .find(|g| g.pci.eq_ignore_ascii_case(&pci))
                    })
                    .map(|g| gpu_name(&g.model))
                    .unwrap_or_else(|| "Physical GPU".into());
                content = content.push(copy(format!("GPU: {model} · {pci}")));
            }
        }
    }
    let actions = row![
        quiet("Go back", Msg::BackToEdit(id)).disabled(app.busy),
        button("Save & restart")
            .on_press(Msg::ConfirmWorkspaceEdit(id))
            .disabled(app.busy || app.snapshot_unavailable)
    ]
    .spacing(8)
    .wrap();
    let mut form = column![heading(format!("Restart {name} to save changes?"))].spacing(16);
    if let Some(error) = panel_error(app) {
        form = form.push(message(error, true));
    }
    (form.push(content).into(), actions.into())
}
