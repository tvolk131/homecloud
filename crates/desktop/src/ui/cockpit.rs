use super::*;
use crate::virtualization::{Cockpit, Event, GuestIdentity, Page as CockpitPage};
use iced::widget::{column, row};

fn navigate(page: CockpitPage) -> Msg {
    Msg::Cockpit(Event::Navigate(page))
}

pub(super) fn view<'a>(app: &'a App, cockpit: &'a Cockpit) -> Element<'a, Msg> {
    responsive(move |size| {
        let rail = navigation_rail(
            [
                (CockpitPage::Overview, "Overview", IconName::Dns),
                (CockpitPage::Guests, "VMs", IconName::DesktopWindows),
                (CockpitPage::Storage, "Storage", IconName::Storage),
                (CockpitPage::Settings, "Settings", IconName::Settings),
            ]
            .into_iter()
            .map(|(page, label, name)| {
                let symbol = Symbol::new(name);
                NavigationItem::new(page, label, symbol).selected_icon(symbol.fill(Fill::Filled))
            }),
            Some(cockpit.page),
        )
        .on_select(navigate)
        .header(container(icon(IconName::Cloud)).center_x(48).center_y(48))
        .footer(
            column![badge("Owner", false), copy("VM power")]
                .align_x(Alignment::Center)
                .spacing(8),
        );
        let title = match cockpit.page {
            CockpitPage::Overview => "Proxmox cockpit",
            CockpitPage::Guests if cockpit.selected_guest.is_some() => "Guest details",
            CockpitPage::Guests => "VMs & templates",
            CockpitPage::Storage => "Storage capacity",
            CockpitPage::Settings => "Settings",
        };
        let mut content = column![
            row![
                column![
                    typography(title, Type::HeadlineLarge),
                    copy("Your infrastructure, over Iroh.")
                ]
                .spacing(6)
                .width(Length::Fill),
                secondary(
                    if app.refreshing { "Refreshing…" } else { "Refresh" },
                    Msg::Refresh,
                )
                .disabled(app.refreshing),
            ]
            .align_y(Alignment::Center)
            .spacing(16),
            row![source(cockpit), badge("Owner power controls", false)]
                .spacing(8)
                .align_y(Alignment::Center),
        ]
        .spacing(20);
        if let Some(error) = &app.error {
            content = content.push(message(error, true));
        }
        if let Some(error) = &cockpit.power_error {
            content = content.push(super::cockpit_power::problem(error));
        }
        if cockpit.operations_stale(crate::unix_now()) {
            content = content.push(surface(
                column![
                    heading("Power controls paused"),
                    copy("A fresh operations list is required before another power action. Retained history is not current progress."),
                    copy(cockpit.operations_error.as_deref().unwrap_or("Waiting for a fresh operations response…")),
                ].spacing(8),
                true,
            ));
        }
        if let Some(recovery) = super::cockpit_power::recovery(cockpit) {
            content = content.push(recovery);
        }
        if let Some(confirmation) = super::cockpit_power::confirmation(cockpit) {
            content = content.push(confirmation);
        }
        if cockpit.stale(crate::unix_now()) && cockpit.inventory.is_some() {
            content = content.push(surface(
                column![
                    heading("Inventory is stale"),
                    copy("Capacity and guest status below are retained observations, not current health."),
                    copy(cockpit.error.as_deref().unwrap_or("No successful refresh in the last 30 seconds.")),
                ]
                .spacing(8),
                true,
            ));
        }
        if cockpit.page == CockpitPage::Settings {
            content = content.push(settings(app)).push(surface(
                column![
                    heading("Owner-only connection"),
                    copy("The server identifies this connection as a virtualization cockpit. Legacy grants, VM creation, SSH, and T3 setup are not available here."),
                    copy("Proxmox credentials stay on the server. Inventory and durable power operations refresh every 5 seconds."),
                    copy("Only non-template QEMU VMs have power controls. Force stop requires confirmation. Unknown operations hold further actions until you inspect Proxmox and explicitly acknowledge the hold; acknowledgement never retries power."),
                    copy(format!("Client profile: {}", app.profile.display())),
                ]
                .spacing(12),
                false,
            ));
        } else if let Some(inventory) = &cockpit.inventory {
            let body = match cockpit.page {
                CockpitPage::Overview => overview(inventory, cockpit.stale(crate::unix_now())),
                CockpitPage::Guests => guests(inventory, cockpit),
                CockpitPage::Storage => storage(inventory),
                CockpitPage::Settings => unreachable!(),
            };
            content = content.push(body);
        } else {
            content = content.push(surface(
                column![
                    heading(if cockpit.error.is_some() { "Inventory unavailable" } else { "Loading inventory" }),
                    copy(cockpit.error.as_deref().unwrap_or("Waiting for the first authenticated inventory response…")),
                    copy("No resource counts or health conclusions are available until a request succeeds."),
                ]
                .spacing(12),
                false,
            ));
        }
        content = content.push(super::cockpit_power::history(cockpit));
        let padding = if size.width < 950.0 { 20.0 } else { 32.0 };
        let pane = scrollable(
            container(content)
                .padding(padding)
                .max_width(1200)
                .width(Length::Fill),
        )
        .id("cockpit-body")
        .width(Length::Fill)
        .height(Length::Fill);
        row![rail, pane].height(Length::Fill).into()
    })
    .into()
}

fn source(cockpit: &Cockpit) -> Element<'static, Msg> {
    badge(
        match cockpit.inventory.as_ref().map(|inventory| inventory.source) {
            Some(VirtualizationSource::Simulated) => "Simulated · no real guests",
            Some(VirtualizationSource::Proxmox) => "Proxmox API",
            None => "Source not yet observed",
        },
        false,
    )
}

fn bytes(value: Option<u64>) -> String {
    match value {
        Some(value) if value >= GIB => format!("{:.1} GiB", value as f64 / GIB as f64),
        Some(value) => format!("{:.1} MiB", value as f64 / (1024.0 * 1024.0)),
        None => "Not reported".into(),
    }
}

fn capacity<'a>(label: &'static str, used: Option<u64>, total: Option<u64>) -> Element<'a, Msg> {
    let mut line = column![
        copy(label),
        typography(
            format!("{} used / {} capacity", bytes(used), bytes(total)),
            Type::TitleMedium,
        )
    ]
    .spacing(8);
    if let (Some(used), Some(total)) = (used, total)
        && total > 0
    {
        line = line.push(iced_m3::linear_progress(
            (used as f64 / total as f64).min(1.0) as f32,
        ));
    }
    line.into()
}

fn overview(inventory: &VirtualizationInventory, stale: bool) -> Element<'_, Msg> {
    let templates = inventory
        .guests
        .iter()
        .filter(|guest| guest.template)
        .count();
    let mut body = column![
        surface(
            column![
                heading(format!("Nodes: {} · Guests: {} · Templates: {}", inventory.nodes.len(), inventory.guests.len(), templates)),
                copy("Visible to this API token. Missing resources may be hidden by Proxmox permissions."),
            ]
            .spacing(10),
            false,
        ),
        heading("Nodes"),
    ]
    .spacing(16);
    for node in &inventory.nodes {
        body = body.push(surface(
            column![
                row![
                    typography(&node.name, Type::TitleLarge).width(Length::Fill),
                    badge(&node.status, !stale && node.status == "online"),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
                copy(format!(
                    "CPU: {} threads · {} usage",
                    node.cpu_threads
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "Not reported".into()),
                    node.cpu_usage_ratio
                        .map(|value| format!("{:.1}%", value * 100.0))
                        .unwrap_or_else(|| "Not reported".into()),
                )),
                capacity("Memory", node.memory_used_bytes, node.memory_capacity_bytes),
            ]
            .spacing(16),
            false,
        ));
    }
    if inventory.nodes.is_empty() {
        body = body.push(copy("No nodes visible to this API token."));
    }
    body.into()
}

fn guest_kind(guest: &VirtualizationGuest) -> &'static str {
    match (guest.kind, guest.template) {
        (VirtualizationGuestKind::VirtualMachine, true) => "VM template",
        (VirtualizationGuestKind::Container, true) => "Container template",
        (VirtualizationGuestKind::VirtualMachine, false) => "Virtual machine",
        (VirtualizationGuestKind::Container, false) => "Container",
    }
}

fn guests<'a>(inventory: &'a VirtualizationInventory, cockpit: &'a Cockpit) -> Element<'a, Msg> {
    let mut body = column![copy("VM names do not establish T3 installation or connection status. T3 management is not yet available in this cockpit.")].spacing(16);
    if let Some(identity) = &cockpit.selected_guest {
        body = body.push(secondary("Back to VMs", navigate(CockpitPage::Guests)));
        let Some(guest) = inventory
            .guests
            .iter()
            .find(|guest| guest.node == identity.node && guest.vmid == identity.vmid)
        else {
            return body.push(surface(
                column![
                    heading("Guest no longer visible here"),
                    copy("It may have moved, been removed, or become hidden by API permissions. No deletion or other action was taken."),
                ].spacing(12),
                false,
            )).into();
        };
        return body.push(surface(
            column![
                heading(guest.name.as_deref().unwrap_or("Unnamed guest")),
                copy(format!("{} · ID {} · node {}", guest_kind(guest), guest.vmid, guest.node)),
                copy(format!("Last reported status: {}", guest.status)),
                copy(format!("Configured vCPUs: {}", guest.cpu_limit.map(|value| value.to_string()).unwrap_or_else(|| "Not reported".into()))),
                copy(format!("Configured memory limit: {}", bytes(guest.memory_limit_bytes))),
                copy(format!("Observed memory use: {}", bytes(guest.memory_used_bytes))),
                copy(format!("Reported disk capacity: {}", bytes(guest.reported_disk_capacity_bytes))),
                copy("Disk capacity is not guest filesystem usage, a backing-space reservation, or a hard quota."),
                super::cockpit_power::controls(cockpit, guest),
            ].spacing(14),
            false,
        )).into();
    }
    for guest in &inventory.guests {
        let details = column![
            typography(
                guest.name.as_deref().unwrap_or("Unnamed guest"),
                Type::TitleLarge
            ),
            copy(format!(
                "{} · ID {} · {}",
                guest_kind(guest),
                guest.vmid,
                guest.node
            )),
            copy(format!("Last reported status: {}", guest.status)),
            copy(format!(
                "Memory limit: {} · Disk capacity: {}",
                bytes(guest.memory_limit_bytes),
                bytes(guest.reported_disk_capacity_bytes)
            )),
            button(format!("Inspect guest {}", guest.vmid))
                .variant(Variant::Outlined)
                .on_press(Msg::Cockpit(Event::Inspect(GuestIdentity {
                    node: guest.node.clone(),
                    vmid: guest.vmid,
                }))),
        ]
        .spacing(12);
        body = body.push(surface(details, false));
    }
    if inventory.guests.is_empty() {
        body = body.push(copy("No guests visible to this API token."));
    }
    body.into()
}

fn storage(inventory: &VirtualizationInventory) -> Element<'_, Msg> {
    let mut body = column![
        copy("Proxmox storage capacity is not ZFS topology, disk health, or a quota. Storage entries may share backing pools; do not add their capacities together."),
    ]
    .spacing(16);
    for storage in &inventory.storage {
        body = body.push(surface(
            column![
                heading(&storage.name),
                copy(format!(
                    "Node: {} · Last reported status: {}",
                    storage.node, storage.status
                )),
                capacity("Storage", storage.used_bytes, storage.capacity_bytes),
            ]
            .spacing(16),
            false,
        ));
    }
    if inventory.storage.is_empty() {
        body = body.push(copy("No storage visible to this API token."));
    }
    body.into()
}
