# VM resource metrics

Homecloud reads the existing Incus metrics endpoint directly from its unprivileged
Rust controller. Incus collects guest CPU, memory, filesystem and interface counters
through its existing `incus-agent`. The controller samples every 15 seconds, caches
the result, and includes only authorized VMs in the desktop's regular snapshots.
No new guest instrumentation, Prometheus server, historical database, or Python
runtime bridge is involved. The Homecloud guest task runner is unchanged.

## Enable on an existing host

First install the matching metrics-capable daemon using the normal software-update
procedure. From a reviewed source checkout or unpacked host bundle, run:

```sh
sudo python3 scripts/install_host.py --metrics-only --check
sudo python3 scripts/install_host.py --metrics-only
sudo systemctl restart homecloudd.service
sudo -u homecloud homecloudd --config /etc/homecloud/control.json check-metrics
```

Fresh installations perform this provisioning automatically. Setup requires Incus,
OpenSSL and `runuser`. It only changes metrics credentials, the metrics listener,
and the `metrics` entry in `/etc/homecloud/control.json`; it does not change hardware
policy, VM definitions, host resource budgets, or the guest runner. Only the
controller needs restarting. Neither Incus nor the VMs need restarting for an
existing host's metrics setup.

The initial default listener is `127.0.0.1:8444`; later setup and maintenance
reuse the configured port unless `--metrics-port PORT` explicitly selects one.
Setup refuses a conflicting existing listener or disabled metrics
authentication instead of overwriting the operator's configuration. Its preflight
is read-only. Before committing configuration it verifies a scrape as `homecloud`,
not as root. On failure it rolls back the listener and trust entry it just added,
without removing pre-existing ones. Read-only preflight also checks an existing
client identity's path and remaining lifetime before maintenance stops services.
If metrics provisioning subsequently fails during a full maintenance reinstall,
the installer still attempts to resume legacy guests and restart the controller.

## Security boundary

- Incus trusts a dedicated certificate with **type `metrics`**, not `client`.
  The controller receives no instance-management permission or Incus Unix-socket
  membership. A reused management-capable certificate is refused.
- The metrics listener is loopback-only, with authentication left enabled.
- The Rust client trusts only the pinned Incus server certificate and performs
  normal TLS certificate and hostname verification. Setup selects a DNS SAN from
  Incus's local, trusted server certificate. The client resolves that name directly
  to `127.0.0.1`, bypasses proxies, and refuses redirects. It never disables TLS
  verification or follows an arbitrary endpoint from guest data.
- Versioned credentials live below `/etc/homecloud/metrics/`, root-owned with
  `homecloud` group access: directories `0750`, identity/config files `0640`.
  The combined `client.pem` contains a private key; do not include it in bundles,
  diagnostics, or source control.
- The admin-only installer provisions credentials once. Continuous sampling makes
  HTTPS requests directly from Rust without sudo or a privileged Python process.
- Samples are bounded and matched only to managed VMs in Incus's default project.
  Administrators can see all managed VMs; members can see only their own metrics.

The control plane's existing privileged host-operation path is unchanged. Metrics
access is deliberately separate from it.

Guest telemetry is informational, not an admission, authorization, or billing
input. The shared Incus exposition is not a cryptographic per-VM provenance
boundary: on Incus versions that do not escape guest-provided label values, a
replaced/malicious guest agent could forge complete metric lines for another known
instance. Malformed stock-agent label output is isolated, but authenticating the
Incus endpoint does not authenticate each metric's original guest producer.

## Display semantics

- **CPU:** percentage of the guest's total vCPU time, normalized to 0–100%, not a
  percentage per physical core. Idle and I/O wait are excluded from busy time.
  The first sample warms up; rates require two samples.
- **Memory:** guest total minus `MemAvailable`, so reclaimable cache is not
  misleadingly counted as unavailable RAM. Guest usable RAM can differ from its
  configured allocation.
- **Root filesystem:** actual guest `/` filesystem capacity, used bytes and
  available bytes. Used is total minus **free**, not total minus available;
  filesystem-reserved blocks are not counted as used. This is not the VM's virtual
  disk allocation, ZFS physical allocation, snapshot size, or a sum of other mounts.
- **Network:** receive/transmit bytes per second for up to 64 guest interfaces, excluding
  loopback. Interfaces are not summed, to avoid double-counting bridges and virtual
  devices. The first sample and reset counters have no rate rather than a fake zero.

Assigned resources remain separate from measured usage. A stopped VM has no live
metrics; transitioning or unobservable VMs are unavailable, not falsely labelled
stopped. A valid guest `/` filesystem sample is required before accepting any VM
sample: Incus's QEMU fallback has CPU and memory counters but no guest filesystem
data. Missing guest-agent data is therefore unavailable rather than host/QEMU
memory being misrepresented as guest usage. Malformed label lines are skipped;
ambiguous or invalid numeric data discards only that VM, not healthy neighbors.
Extra network interfaces are truncated rather than failing the scrape.
Failed scrapes retain the last gauges as stale and
clear rates; observations also become stale after 45 seconds without a fresh scrape.
Counter resets, CPU topology changes and long gaps restart rate warmup. Cache state
is in memory and resets with the controller; it is not historical monitoring.
The GUI trusts the controller's freshness state instead of comparing clocks
across machines; timestamp ages remain relative to the desktop's clock.

## Renewal and recovery

Client certificates last 365 days. Renew before expiry (setup refuses reuse within
seven days of expiry). There is not yet an advance-expiry badge in the GUI;
schedule renewal operationally rather than waiting for collection to fail:

```sh
sudo python3 scripts/install_host.py --metrics-only --rotate-metrics-client
sudo systemctl restart homecloudd.service
sudo -u homecloud homecloudd --config /etc/homecloud/control.json check-metrics
sudo incus config trust list
```

After successful verification, revoke the **previous** `homecloud-metrics-*`
fingerprint with `sudo incus config trust remove OLD_FINGERPRINT`, and remove only
its old credential-version directory. Rotation intentionally retains the previous
identity until the running controller has loaded the new configuration.

If Incus's server certificate changes, rerun `--metrics-only` and restart the
controller to repin it through the trusted local API. Do not work around certificate
errors by disabling verification or authentication. Inspect `journalctl -u
homecloudd` for collection failures. A metrics outage never stops lifecycle or
application reconciliation.
