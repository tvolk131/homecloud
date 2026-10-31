use super::*;
use crate::visual_tests::{fixture as ordinary, id, lock, simulator};

fn fixture(state: VmMetricsState) -> App {
    let mut app = ordinary();
    app.page = Page::Workspace(id(10));
    app.snapshot.as_mut().unwrap().vm_metrics = vec![VmMetrics::empty(id(10), state)];
    app
}

pub(super) fn sampled_fixture() -> App {
    let mut app = fixture(VmMetricsState::Ready);
    let metrics = &mut app.snapshot.as_mut().unwrap().vm_metrics[0];
    metrics.observed_at = Some(unix_now());
    metrics.attempted_at = metrics.observed_at;
    metrics.cpu_percent = Some(25.5);
    metrics.memory = Some(VmMemoryUsage {
        total_bytes: 4 * GIB,
        available_bytes: GIB,
    });
    metrics.root_filesystem = Some(VmFilesystemUsage {
        total_bytes: 40 * GIB,
        free_bytes: 10 * GIB,
        available_bytes: 8 * GIB,
    });
    metrics.network = vec![VmNetworkUsage {
        interface: "ens3".into(),
        received_bytes: 12 * 1024 * 1024,
        sent_bytes: 6 * 1024 * 1024,
        receive_bytes_per_second: Some(64.0 * 1024.0),
        send_bytes_per_second: Some(2.0 * 1024.0),
    }];
    app
}

#[test]
fn vm_metrics_ready_usage_is_distinct_from_allocation_and_excludes_reserved_disk_blocks() {
    let _guard = lock();
    let mut app = sampled_fixture();
    let mut other = VmMetrics::empty(id(11), VmMetricsState::Ready);
    other.observed_at = Some(unix_now());
    other.cpu_percent = Some(99.0);
    app.snapshot.as_mut().unwrap().vm_metrics.insert(0, other);
    let mut ui = simulator(&app, (1180.0, 1800.0));
    for label in [
        "Assigned capacity",
        "4 GiB",
        "1 core",
        "40 GiB",
        "Guest usage",
        "Current sample",
        "25.5%",
        "3.0 GiB / 4.0 GiB used",
        "1.0 GiB available",
        "30.0 GiB / 40.0 GiB used",
        "8.0 GiB available to applications",
        "ens3",
        "RX · received",
        "64.0 KiB/s",
        "12.0 MiB total received",
        "TX · sent",
        "2.0 KiB/s",
        "6.0 MiB total sent",
    ] {
        assert!(ui.find(label).is_ok(), "Missing {label}");
    }
    assert!(ui.find("32.0 GiB / 40.0 GiB used").is_err());
    assert!(ui.find("99.0%").is_err());
    assert!(
        ui.find("Assigned capacity").unwrap().bounds().y
            < ui.find("Guest usage").unwrap().bounds().y
    );
}

#[test]
fn vm_metrics_empty_states_never_substitute_allocations_or_zero_usage() {
    let _guard = lock();
    for (state, label) in [
        (VmMetricsState::NotConfigured, "Metrics not configured"),
        (VmMetricsState::Waiting, "Waiting for metrics"),
        (VmMetricsState::Ready, "Waiting for metrics"),
        (VmMetricsState::Stale, "Stale sample"),
        (VmMetricsState::Unavailable, "Metrics unavailable"),
        (VmMetricsState::Stopped, "VM stopped"),
        (VmMetricsState::Unknown, "Metrics unavailable"),
    ] {
        let app = fixture(state);
        let mut ui = simulator(&app, (1180.0, 1800.0));
        assert!(ui.find(label).is_ok(), "state={state:?}");
        for absent in ["Current sample", "CPU usage", "0.0%", "0 B/s"] {
            assert!(ui.find(absent).is_err(), "state={state:?}, label={absent}");
        }
        assert!(ui.find("Assigned capacity").is_ok());
    }
    let mut app = ordinary();
    app.page = Page::Workspace(id(10));
    assert!(
        simulator(&app, (1180.0, 1800.0))
            .find("Metrics not configured")
            .is_ok()
    );
    app.page = Page::Workspace(id(11));
    assert!(simulator(&app, (1180.0, 1800.0)).find("VM stopped").is_ok());
}

#[test]
fn vm_metrics_first_sample_shows_memory_and_counters_while_rates_warm_up() {
    let _guard = lock();
    let mut app = sampled_fixture();
    let metrics = &mut app.snapshot.as_mut().unwrap().vm_metrics[0];
    metrics.cpu_percent = None;
    metrics.network[0].receive_bytes_per_second = None;
    metrics.network[0].send_bytes_per_second = None;
    let mut ui = simulator(&app, (1180.0, 1800.0));
    for label in [
        "Current sample",
        "Warming up",
        "3.0 GiB / 4.0 GiB used",
        "12.0 MiB total received",
        "6.0 MiB total sent",
    ] {
        assert!(ui.find(label).is_ok(), "Missing {label}");
    }
    assert!(ui.find("0.0%").is_err());
    assert!(ui.find("0 B/s").is_err());
}

#[test]
fn vm_metrics_retained_samples_are_never_presented_as_current() {
    let _guard = lock();
    for (state, offline, age, label) in [
        (VmMetricsState::Stale, false, 15, "Stale sample"),
        (
            VmMetricsState::Unavailable,
            false,
            15,
            "Metrics unavailable",
        ),
        (VmMetricsState::Ready, true, 15, "Snapshot unavailable"),
    ] {
        let mut app = sampled_fixture();
        app.snapshot_unavailable = offline;
        let metrics = &mut app.snapshot.as_mut().unwrap().vm_metrics[0];
        metrics.state = state;
        metrics.observed_at = Some(unix_now() - age);
        let mut ui = simulator(&app, (1180.0, 1800.0));
        assert!(ui.find(label).is_ok(), "state={state:?}, offline={offline}");
        assert!(ui.find("Current sample").is_err());
        assert!(ui.find("30.0 GiB / 40.0 GiB used").is_ok());
        assert!(ui.find("12.0 MiB total received").is_ok());
        assert!(ui.find("Warming up").is_err());
    }
}

#[test]
fn vm_metrics_trust_host_freshness_across_clock_skew_and_hide_stopped_usage() {
    let _guard = lock();
    let mut app = sampled_fixture();
    for timestamp in [unix_now() + 3600, unix_now() - 3600] {
        app.snapshot.as_mut().unwrap().vm_metrics[0].observed_at = Some(timestamp);
        let mut ui = simulator(&app, (1180.0, 1800.0));
        assert!(ui.find("Current sample").is_ok());
        assert!(ui.find("Sample time uncertain").is_err());
        assert!(ui.find("Stale sample").is_err());
    }
    app.snapshot.as_mut().unwrap().vms[0].observed = "stopped".into();
    let mut ui = simulator(&app, (1180.0, 1800.0));
    assert!(ui.find("VM stopped").is_ok());
    assert!(ui.find("CPU usage").is_err());
    assert!(ui.find("25.5%").is_err());
}

#[test]
fn vm_metrics_partial_samples_invalid_rates_and_idle_rates_remain_distinct() {
    let _guard = lock();
    let mut app = sampled_fixture();
    let metrics = &mut app.snapshot.as_mut().unwrap().vm_metrics[0];
    metrics.cpu_percent = Some(f64::NAN);
    metrics.memory = None;
    metrics.root_filesystem = None;
    metrics.network[0].receive_bytes_per_second = Some(f64::INFINITY);
    metrics.network[0].send_bytes_per_second = Some(-1.0);
    let mut ui = simulator(&app, (1180.0, 1800.0));
    assert!(ui.find("Not reported").is_ok());
    for absent in ["NaN%", "inf B/s", "-1 B/s", "Warming up", "0.0%"] {
        assert!(ui.find(absent).is_err(), "Unexpected {absent}");
    }
    drop(ui);
    let metrics = &mut app.snapshot.as_mut().unwrap().vm_metrics[0];
    metrics.cpu_percent = Some(0.0);
    metrics.network[0].receive_bytes_per_second = Some(0.0);
    metrics.network[0].send_bytes_per_second = Some(0.0);
    let mut ui = simulator(&app, (1180.0, 1800.0));
    assert!(ui.find("0.0%").is_ok());
    assert!(ui.find("0 B/s").is_ok());
}

#[test]
fn vm_metrics_narrow_layout_stacks_usage_and_preserves_each_network_interface() {
    let _guard = lock();
    let mut app = sampled_fixture();
    app.snapshot.as_mut().unwrap().vm_metrics[0]
        .network
        .push(VmNetworkUsage {
            interface: "enp2s0".into(),
            received_bytes: 512,
            sent_bytes: 256,
            receive_bytes_per_second: None,
            send_bytes_per_second: Some(0.5),
        });
    for width in [620.0, 800.0, 1180.0] {
        let mut ui = simulator(&app, (width, 2000.0));
        let cpu = ui.find("CPU usage").unwrap().bounds();
        let memory = ui.find("Memory usage").unwrap().bounds();
        let root = ui.find("Root filesystem").unwrap().bounds();
        if width < 1000.0 {
            assert!(cpu.y < memory.y && memory.y < root.y);
        } else {
            assert_eq!(cpu.y, memory.y);
            assert_eq!(memory.y, root.y);
        }
        for label in [
            "30.0 GiB / 40.0 GiB used",
            "8.0 GiB available to applications",
            "ens3",
            "enp2s0",
            "512 B total received",
            "256 B total sent",
            "0.5 B/s",
        ] {
            let bounds = ui.find(label).unwrap().bounds();
            assert!(bounds.width > 0.0, "width={width}, label={label}");
            assert!(bounds.x >= 0.0 && bounds.x + bounds.width <= width);
        }
    }
}
