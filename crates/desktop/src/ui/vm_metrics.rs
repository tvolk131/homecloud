use super::*;
use iced::widget::{column, row};

pub(super) fn view<'a>(app: &App, snapshot: &'a Snapshot, vm: &Vm, width: f32) -> Element<'a, Msg> {
    let metrics = snapshot
        .vm_metrics
        .iter()
        .find(|sample| sample.vm_id == vm.id);
    let state = if vm.observed == "stopped" {
        VmMetricsState::Stopped
    } else {
        metrics.map_or(VmMetricsState::NotConfigured, |sample| sample.state)
    };
    let now = unix_now();
    let observed = metrics.and_then(|sample| sample.observed_at);
    let current = !app.snapshot_unavailable && state == VmMetricsState::Ready && observed.is_some();
    let (status, explanation) = if app.snapshot_unavailable {
        (
            "Snapshot unavailable",
            "Connection lost. Retained usage is from the last snapshot, not current.",
        )
    } else {
        match state {
            VmMetricsState::NotConfigured => (
                "Metrics not configured",
                "Guest metrics are not configured for this VM. Assigned capacity is not actual usage.",
            ),
            VmMetricsState::Waiting => (
                "Waiting for metrics",
                "Waiting for the first guest sample. CPU and network rates need two samples.",
            ),
            VmMetricsState::Ready if current => (
                "Current sample",
                "Sampled about every 15 seconds. CPU and network rates need two samples.",
            ),
            VmMetricsState::Ready if observed.is_none() => (
                "Waiting for metrics",
                "Waiting for a timestamped guest sample.",
            ),
            VmMetricsState::Ready | VmMetricsState::Stale => (
                "Stale sample",
                "Showing the last successful sample, not current usage.",
            ),
            VmMetricsState::Unavailable => (
                "Metrics unavailable",
                "Guest metrics could not be read. Any retained values are from the last successful sample.",
            ),
            VmMetricsState::Stopped => ("VM stopped", "No current usage while the VM is stopped."),
            VmMetricsState::Unknown => (
                "Metrics unavailable",
                "This host reports a metrics state this app does not recognize.",
            ),
        }
    };
    let mut content = column![
        row![
            typography("Guest usage", Type::TitleMedium),
            badge(status, current)
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .wrap(),
        copy(explanation),
    ]
    .spacing(12);
    if let Some(observed) = observed {
        content = content.push(copy(format!("Last sample: {}", age(observed, now))));
    }
    if let Some(attempted) = metrics.and_then(|sample| sample.attempted_at) {
        content = content.push(copy(format!("Last attempt: {}", age(attempted, now))));
    }
    let Some(sample) = metrics.filter(|_| {
        observed.is_some()
            && matches!(
                state,
                VmMetricsState::Ready | VmMetricsState::Stale | VmMetricsState::Unavailable
            )
    }) else {
        return content.into();
    };
    let cpu = sample
        .cpu_percent
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| missing_rate(sample.cpu_percent, current).into());
    let memory = sample.memory.as_ref().filter(|usage| usage.total_bytes > 0);
    let root = sample
        .root_filesystem
        .as_ref()
        .filter(|usage| usage.total_bytes > 0);
    let usage = [
        stat("CPU usage", cpu, "of assigned CPU".into()),
        stat(
            "Memory usage",
            memory.map_or_else(
                || "Not reported".into(),
                |usage| used_total(usage.used_bytes(), usage.total_bytes),
            ),
            memory.map_or_else(String::new, |usage| {
                format!("{} available", bytes(usage.available_bytes as f64))
            }),
        ),
        stat(
            "Root filesystem",
            root.map_or_else(
                || "Not reported".into(),
                |usage| used_total(usage.used_bytes(), usage.total_bytes),
            ),
            root.map_or_else(String::new, |usage| {
                format!(
                    "{} available to applications",
                    bytes(usage.available_bytes as f64)
                )
            }),
        ),
    ];
    let usage: Element<'_, Msg> = if width < 740.0 {
        widget::Column::with_children(usage).spacing(16).into()
    } else {
        widget::Row::with_children(usage).spacing(20).into()
    };
    content = content
        .push(usage)
        .push(typography("Network", Type::TitleSmall));
    if sample.network.is_empty() {
        content = content.push(copy("No network interfaces reported."));
    }
    for interface in &sample.network {
        let directions = [
            stat(
                "RX · received",
                rate(interface.receive_bytes_per_second, current),
                format!("{} total received", bytes(interface.received_bytes as f64)),
            ),
            stat(
                "TX · sent",
                rate(interface.send_bytes_per_second, current),
                format!("{} total sent", bytes(interface.sent_bytes as f64)),
            ),
        ];
        let directions: Element<'_, Msg> = if width < 440.0 {
            widget::Column::with_children(directions).spacing(12).into()
        } else {
            widget::Row::with_children(directions).spacing(20).into()
        };
        content = content.push(
            column![
                typography(interface.interface.as_str(), Type::TitleSmall),
                directions
            ]
            .spacing(8),
        );
    }
    content.into()
}

fn stat(label: &'static str, value: String, detail: String) -> Element<'static, Msg> {
    let mut content = column![copy(label), typography(value, Type::TitleMedium)].spacing(4);
    if !detail.is_empty() {
        content = content.push(copy(detail));
    }
    content.width(Length::Fill).into()
}

fn used_total(used: u64, total: u64) -> String {
    format!("{} / {} used", bytes(used as f64), bytes(total as f64))
}

fn missing_rate(value: Option<f64>, current: bool) -> &'static str {
    if value.is_none() && current {
        "Warming up"
    } else {
        "Not reported"
    }
}

fn rate(value: Option<f64>, current: bool) -> String {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| format!("{}/s", bytes(value)))
        .unwrap_or_else(|| missing_rate(value, current).into())
}

fn bytes(mut value: f64) -> String {
    let mut unit = "B";
    for next in ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if unit == "B" && value.fract() == 0.0 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn age(timestamp: u64, now: u64) -> String {
    if timestamp > now {
        return "ahead of this device's clock".into();
    }
    match now - timestamp {
        0..15 => "just now".into(),
        seconds @ 15..60 => format!("{seconds}s ago"),
        seconds @ 60..3600 => format!("{} min ago", seconds / 60),
        seconds @ 3600..86400 => format!("{} h ago", seconds / 3600),
        seconds => format!("{} days ago", seconds / 86400),
    }
}
