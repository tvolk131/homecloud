# Disposable real-PVE task-tracked power validation

Date: November 2, 2026. Physical target: authorized `resolvr-box`, which remains
Ubuntu/Incus. This experiment does not install Proxmox on that physical host,
migrate workloads, or deploy the new controller into production.

## Ownership and bounded execution

The power runner imports `proxmox_cockpit_live.py` and wraps the existing host
and guest modules in memory. VM preparation, Debian/PVE installation, baseline
capture, isolated Rust compilation, and cleanup are the existing ownership-safe
implementations, not a duplicated physical-host harness. The original lab
files, Rust crates, cockpit evidence, and previous reports are not edited by
the power validation.

Every run uses a new UUID and private mode-0600 manifest. The disposable Incus
VM, ZFS dataset, storage, network, and transient build unit derive from that
UUID. Each native inner-VM inspection checks both the outer Incus ownership tag
and the inner `description: Owned disposable run <UUID>` before touching the
seeded nested VM IDs 7101 and 7199. Power actions target only VM 7101. VM 7199
is the existing stopped template used for a protection test; no original host
VM or original storage is powered off, rewritten, or removed.

The existing fixture temporarily stops `homecloudd` and adds one owned CPU-policy
entry. It leaves the original VMs' power states and configurations unchanged.
Cleanup restores controller activity and CPU policy, deletes only tagged
resources, removes remote source/build/credentials/manifests, and compares an
independently collected after-baseline. A new bounded probe unit runs as the
unprivileged host user, with a 140-second runtime ceiling, 1 GiB memory limit,
128-task limit, and housekeeping CPU affinity. The existing build unit uses
three jobs, 6 GiB, 512 tasks, and private Cargo/OpenSSL directories. No physical
host package is installed.

After the owned outer VM is prepared, the corrected runner overlaps the bounded
build with the nested PVE installation. Validation waits for both to finish.
The source snapshot is taken at build submission; its per-file hashes describe
the tested archive, not an assumed later state of the shared checkout. Both the
probe and daemon executables, a successful test result, and the official-client
dependency report must exist before a build is accepted as complete.

All fixture/build/probe failures enter `finally` cleanup. A failure stopping the
probe does not skip the existing fixture cleanup. If the entire local process
is externally killed or SSH is lost, use the retained manifest's cleanup mode;
never invent a new run identity to remove resources.

## Reproduction

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s lab -p 'test_proxmox*.py' -v
PYTHONDONTWRITEBYTECODE=1 python3 lab/proxmox_power_live.py run \
  --state /tmp/homecloud-proxmox-power-live-20261102-b.json
PYTHONDONTWRITEBYTECODE=1 python3 lab/proxmox_power_live.py report \
  --state /tmp/homecloud-proxmox-power-live-20261102-b.json \
  --previous-state /tmp/homecloud-proxmox-power-live-20261102-a.json \
  --output lab/evidence/proxmox-power-live.json
```

Use a fresh manifest filename for another run. Existing manifests are cleanup-
only, except that `report` may export their sanitized evidence. Recovery:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 lab/proxmox_power_live.py cleanup \
  --state /tmp/homecloud-proxmox-power-live-20261102-b.json
```

The pinned SSH configuration is
`~/.config/homecloud/host-access/resolvr-box/ssh_config`, alias `hclab-host`.
Sandbox SSH or download failures require explicit escalation; lab authorization
does not bypass that boundary. The evidence exporter refuses publication until
owned resources and transient units are absent and the host baseline is checked.

## Validation method

A new four-hour privilege-separated power token receives inventory-only
`PVEAuditor` auditing at `/`, plus `VM.Audit VM.PowerMgmt` on `/vms/7101` for both
the user and token. It receives no root-wide power permission. The original
read-only token remains a negative case. Token values stay in mode-0600 files
inside the disposable guest or owned private remote build directory. Output is
checked for token values before export. Baseline guest configuration, enrollment
secrets, token values, and public deployment metadata are not published.

The probe uses the production official-client adapter, `Service`, SQLite
`Store`, authenticated loopback Iroh QUIC, and `Client` APIs. Each phase is a
separate process. The shutdown submission process exits with a persisted Running
operation and real UPID; a fresh process, service, endpoint, backend, and reopened
store resume tracking. Independent `qm status`, root PVE task listings including
active tasks, and `/nodes/<node>/tasks/<UPID>/status` responses cross-check the
operation journal. Exactly three new native tasks are expected: `qmstart`,
`qmshutdown`, and the separately requested `qmstop`. Reusing a request key must
not create an extra task.

The bare no-OS seed is deliberately retained rather than downloading a large
guest image. Its graceful shutdown may time out while the VM remains running.
That is a task failure, not permission to force-stop. The test then submits a
separate explicit ForceStop operation and verifies native stopped state.
Template, LXC-kind, reader HTTP 403, invalid-token, non-owner, unknown-device,
and revoked-device checks must not create native power tasks or change state.

The interrupted-Submitting test seeds a durable record directly, without
calling the backend. Restart must convert it to Unknown, block a new action,
allow an owner acknowledgement, and never dispatch on idempotent replay. This
tests the service/store safety boundary; it is not evidence of a real network
timeout or a lost HTTP response during submission.

## Results

Live results and independent cleanup are recorded in the separately generated
`lab/evidence/proxmox-power-live.json`. Exact run identity, source/probe hashes,
native task identities, runtime versions, operation phases, and baseline equality
are taken from that run, not copied from previous cockpit evidence.

### Successful power run

Run `658284a90ef649828290b9389375076e` passed all ten probe phases. It ran from
November 2, 2026, 23:57:17 UTC through November 3, 2026, 00:13:51 UTC; both are
November 2 in the lab's America/Chicago timezone. The nested runtime was
`pve-manager/9.2.25/88a31f5fd94aab6d`, kernel `7.0.14-26-pve`. The production
probe ran as UID 1000, not root. The physical host remained Ubuntu/Incus.

- **Start passed:** Submitting → Running → Succeeded, with an accepted real
  `qmstart` UPID and independent `qm status 7101` reporting running. Immediate
  duplicate submission and terminal replay reused one durable operation.
- **Restart tracking passed:** graceful shutdown had a persisted Running
  `qmshutdown` UPID while the first process exited. A new process, service,
  backend, endpoint, and reopened store resumed that same UPID. Replaying the
  request key did not dispatch another task.
- **Graceful failure was truthful:** the native task ended after 61 seconds
  with `VM quit/powerdown failed - got timeout`; the journal became Failed and
  native VM 7101 remained running. No automatic force-stop occurred.
- **Explicit ForceStop passed:** a separately requested `qmstop` task succeeded
  and VM 7101 became stopped. VM 7199 stayed stopped throughout. The native task
  archive contained exactly three new power tasks: one each of `qmstart`,
  `qmshutdown`, and `qmstop`. Journal counts after the three actions were 1, 2,
  and 3, despite duplicate submissions and replays.
- **Privilege/authentication negatives passed:** the original reader token's
  power request became Failed with a 403 permission rejection and no UPID;
  replay stayed Failed. An invalid token failed inventory authentication and
  produced a Failed record without dispatch. Both left native VM states and
  the power-task set unchanged.
- **Protection/authorization passed:** template start failed before backend
  submission; the LXC-kind request was denied. Member, unknown-device, and
  revoked-device power/list checks were denied, as were the exercised
  non-owner acknowledgement requests. No negative case created a native task
  or changed the stopped target/template state.
- **Safety hold passed:** the deliberately seeded Submitting record became
  Unknown on service recreation, blocked a new action, and became Acknowledged
  only on the owner request. Replaying its original key did not dispatch.

All 40 offline `test_proxmox*.py` tests passed, including 14 new power-harness
tests. The isolated Linux build passed all 15 official-adapter library tests,
compiled the production daemon and power probe, and verified both executable
artifacts. The probe binary's build SHA-256 matched the binary used for live
validation. The official client remained pinned to
`89c1d597a9d15743049eadcb1add380573265904`. All 92 archived input-source hashes
and all four new runner/host/guest/probe hashes matched the checkout at final
verification; the evidence retains the actual tested input snapshot hashes.

### Cleanup result

Cleanup restored all 14 independently collected baseline fields exactly, with
no image-cache exception. The original three VM power states remained
Stopped/Running/Running, their configurations/profiles/devices were unchanged,
`homecloudd` was active again, the CPU policy content and mode were restored,
and `hclab` remained ONLINE with its original parent properties. Physical
package-database, CA-bundle, OpenSSL content/ownership/mode, and root rustup
existence checks matched.

The owned Incus worker, storage, network, ZFS dataset, remote build/credentials,
and host manifest were absent. The worker, build, and power-probe transient
units all reported `not-found`. The outer disposable worker stopped gracefully;
cleanup did not need its force-stop fallback. A second independent pinned-SSH
cleanup verification is retained in the evidence. No PVE instance, controller
deployment, physical migration, build process, or test service was left running.

### Preparatory attempt

Run `818e229cf0244f30befd8ff2a87f620d` installed and seeded disposable real PVE,
but did not execute the power probe. Review found that the new driver attempted
to deserialize `Profile` directly; the established credential-safe interface is
`Profile::load`. Only the new lab driver was corrected. Rather than rewrite an
in-flight source snapshot, the owned build unit was stopped and the run allowed
to enter its existing failure/cleanup path.

Stopping that transient unit exposed a harness-reporting edge case: the original
build helper returned success after cancellation despite no executable having
been produced. Validation rejected the missing binary. The new wrapper now
requires both executable artifacts, a completed test result, and dependency
proof; a regression test covers cancelled builds. This is not evidence of a
production VM-power failure or a completed compilation. No new power action
reached PVE in this attempt.

The preparatory attempt restored the exact independently recorded baseline.
All owned lab resources, build directories, manifests, and transient units were
absent. The private manifest is retained for audit and its failed assessment,
tested snapshot hashes, and cleanup appear under `previous_runs` in the new
power evidence. Earlier cockpit/spike evidence and their hashes stay untouched.

Unchanged earlier evidence SHA-256 values, checked before and after this work:

- `proxmox-cockpit-live.json`:
  `3be502ec76d56c1fd54148c91fa3bba3c7911abd4c5ef53051b704b535f43c94`.
- `proxmox-spike.json`:
  `37c717d0a116d36708bb8efbfeae19876af94f55324d768613b6dea68979d290`.

## Limits

This is real nested PVE and real QEMU power management, not a mock or a physical
PVE migration. No OS-backed successful graceful shutdown, real LXC power action,
multi-node topology, remote desktop-to-lab Iroh relay, public TLS lifecycle,
hard controller-process crash during POST, packet-loss injection, or automatic
recovery after an ambiguous response is claimed. A safety-hold acknowledgement
does not itself perform another action. A real terminal UPID failure is checked
against native PVE status, rather than interpreted as a successful stop.

## New files

- `lab/proxmox_power_live.py`: manifest supervisor, bounded execution, assessment,
  cleanup gating, and scrubbed evidence export with failed-attempt history.
- `lab/proxmox_power_host.py`: wrappers around the existing build/ownership
  helpers, unprivileged probe units, and independent native task/state checks.
- `lab/proxmox_power_guest.py`: owned-fixture checks and VM-scoped power token.
- `lab/proxmox_power_probe.rs`: production Client/Service/Store/adapter driver.
- `lab/test_proxmox_power_live.py`: 14 offline safety/assessment regressions.
- `lab/evidence/proxmox-power-live.json`: new sanitized run evidence.
- `docs/proxmox-power-live-validation.md`: this report. No existing lab file,
  Rust crate, README, other document, branch, or commit was changed by this work.
