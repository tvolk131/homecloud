# Task-tracked Proxmox VM power controls

The owner-only virtualization cockpit can start a QEMU VM, request graceful
shutdown, or explicitly force-stop it. Proxmox still owns VM execution. Homecloud
stores the request and tracks its native Proxmox task; it does not introduce a
second hypervisor, a scheduling policy, or tenant resource contracts.

This extends the [inventory foundation](proxmox-cockpit.md). It does not create
VMs, clone templates, install T3, forward SSH, modify disks, or manage ZFS.
Templates and LXC containers are excluded from power operations in this slice.
Use a matching daemon and desktop build; installed binaries are not upgraded
automatically by changing this checkout.

## Permissions and identity

The original audit-only PVE token cannot perform power actions. Keep the
inventory-audit permissions and deliberately grant `VM.PowerMgmt` only on the
VM paths the owner intends to control, for example `/vms/7101`. Both the PVE
user and its privilege-separated token must have the intended permissions.
Reading another user's task can also require audit permissions; retain the
inventory-audit role rather than replacing it with power-only access.

Do not grant PVE administrator/root access just to make these controls work.
PVE's power privilege is broader than the three exposed Homecloud operations;
Homecloud's allowlist is not a replacement for least-privilege PVE credentials.
Credentials stay in the server's private secret file. HTTPS and the existing
certificate-trust rules remain required, with no insecure transport option.

The daemon continues to require an enrolled active administrator. Coworker
invitation, grants, legacy VM mutations, and host power-off remain disabled in
virtualization mode. Each power request identifies a native node, VM ID, and
QEMU kind. Before dispatch, the server rechecks authorization and visible guest
state and refuses templates, moved/missing guests, unsupported states, and LXC.
The Proxmox API remains authoritative if state changes after that preflight.

Production backends bind the journal to their source and endpoint in the state
directory. Switching from simulation to Proxmox, or to a different endpoint,
requires a separate state directory; old operation records must not be silently
reinterpreted as requests for another host. Token rotation at the same endpoint
does not change this binding.

## Operation lifecycle

1. Persist the request and its UUID idempotency key as `submitting` before any
   power request is dispatched. At most one active/uncertain request may occupy
   a node/VM pair. A key cannot be reused with a different actor or payload.
2. A background dispatch performs one power submission. The accepted native
   UPID is checked against the requested node, VM ID, and action, then persisted
   as `running`. Losing the laptop connection does not cancel that dispatch.
   A failed result write is retried up to three times without another power
   submission. If all writes fail, the controller logs the operation, guest and
   any accepted UPID for independent inspection; the guest remains blocked.
3. The daemon polls known tasks every two seconds while serving. Only a stopped
   task with exit status `OK` becomes `succeeded`; another terminal exit status
   becomes `failed`. This is task completion, not a promise that a cached
   inventory observation has already caught up with the new guest state.
4. Temporary task-read failures retain the UPID and `running` state with an
   error, rather than claiming success or resubmitting. A controller restart
   resumes polling known tasks. The GUI refreshes observations independently.

Graceful shutdown sends `timeout=60` and `forceStop=0`. A guest that does not
respond to shutdown can fail the task while remaining running. Homecloud never
automatically turns that failure into force-stop. Force-stop is a separate
owner decision with a data-loss warning and confirmation.

The recent-history response includes up to 100 records, with active/uncertain
operations first; at most 32 may be active. Older UUIDs remain deduplicated in
the persistent journal even if they no longer appear in the recent list.

## Ambiguous submissions and recovery

A submission timeout, lost response, server-side HTTP failure, or invalid task
identity is not evidence that no VM action occurred. These become `unknown`
safety holds, not automatic retries. A definite authentication/4xx rejection
(except HTTP 408) becomes `failed`. If the controller restarts with a
`submitting` record, that record becomes `unknown`; startup never dispatches it
again. The desktop likewise must not manufacture a new key to retry a lost RPC.

An authenticated controller rejection releases the desktop's local pending
request and leaves an actionable error visible. Transport failures and unexpected
replies remain uncertain. For an HTTP-success Proxmox response, only an explicit
failure envelope with a non-timeout 4xx status is a definite rejection; malformed
or incomplete envelopes retain the safety hold.

The desktop retains an unresolved request key in memory during that app session;
there is no on-disk client power outbox. On app restart it fetches the server's
durable operations without replaying actions. If no record is found after a lost
request, that absence alone is not proof of rejection; inspect Proxmox before
deciding to issue a new action.

For an unknown operation, inspect native Proxmox task history and the guest
before explicitly acknowledging the uncertainty. Acknowledgement records
`acknowledged` and releases the hold. It does **not** establish success, undo an
action, cancel a possible upstream task, or retry it. Only after that inspection
should the owner decide whether a new action is appropriate.

A known task whose status stays inaccessible remains tracked and blocks
conflicting actions. Repair connectivity/audit permissions and inspect Proxmox;
general recovery for permanently lost or expired upstream task history is not
implemented in this slice. Operation records are not backups or restore points.

## CLI

Use the isolated virtualization profile, not the default Incus profile:

```sh
homecloud --profile "$STATE/client.json" virtualization-operations
REQUEST_ID="$(uuidgen)"
homecloud --profile "$STATE/client.json" virtualization-power pve 101 start \
  --request-id "$REQUEST_ID"
```

Retain the UUID before submission. If the response is lost, first inspect the
operation list. Reusing the **same UUID and exact request** retrieves an existing
record without another dispatch. A different UUID is a new action, not recovery.
Use `shutdown` for graceful shutdown. `force-stop` additionally requires
`--confirm-force-stop`. Releasing an unknown hold requires both its operation ID
and confirmation of independent inspection:

```sh
homecloud --profile "$STATE/client.json" virtualization-acknowledge "$REQUEST_ID" \
  --inspected-proxmox
```

## Validation scope

On November 2, 2026 (America/Chicago), the
[disposable real-PVE power validation](proxmox-power-live-validation.md) passed
against PVE 9.2.25 using the pinned official Rust client as UID 1000. Start and
explicit force-stop reached successful native UPIDs and the expected native VM
states. Reopened Service/Store tracking resumed the same shutdown UPID, while
duplicate requests and terminal replays did not create more tasks. Exactly
three native power tasks occurred: start, graceful shutdown, and force-stop.

The no-OS fixture did not respond to graceful shutdown: its task failed after
61 seconds and the VM remained running until the separate force-stop request.
This validates truthful failure handling and the absence of an automatic
force-stop, not successful OS-backed graceful shutdown. Read-only 403 rejection,
authorization, template/LXC-kind guards, and synthetic Unknown acknowledgement
also passed without unintended power changes. Real lost-response injection and
hard controller-process failure during POST remain untested.

All 40 lab tests and 15 Linux adapter tests passed. All 14 physical-host baseline
fields were restored exactly; original VM configurations/power states were
unchanged, and owned lab resources, builds, manifests, and transient units were
removed. The preparatory driver-only failure and its cleanup are recorded,
not counted as a successful runtime validation. Sanitized source hashes,
native tasks, phases, and independent cleanup checks are in
`lab/evidence/proxmox-power-live.json`.

Focused tests exercise owner-only access, request-key deduplication, concurrent
reservation, per-guest exclusion, journal persistence, startup uncertainty,
acknowledgement, task failures, temporary polling failures, template/LXC/state
guards, and authenticated loopback Iroh. Linux adapter fixtures check HTTP
methods and paths, native UPIDs, safe shutdown parameters, terminal task status,
rejection versus ambiguity, and credential-safe errors. Desktop controls and
disposable real-PVE behavior have their own regression and live evidence.

The desktop suite has 36 focused cockpit tests and 14 cockpit screenshot
baselines covering eligible controls, force-stop confirmation, lost-response
recovery, running/failed/unknown tasks, acknowledgement, and unavailable
operations, including 800×600 layouts. The broader suite also covers the legacy
Incus experience. Run the full workspace with the software renderer:

```sh
ICED_TEST_BACKEND=tiny-skia cargo test --locked --offline --workspace -- --test-threads=1
cargo fmt --all -- --check
python3 -m unittest discover -s lab -p 'test_proxmox*.py' -v
```

No physical host migration, installed app/daemon upgrade, disk enforcement,
VM provisioning, recovery-from-host-loss, or laptop-to-host relay/NAT guarantee
is implied by these power controls.

### November 6 review follow-up

Independent adapter/security and desktop/lab reviews were followed by fixes for
authenticated-rejection recovery, confirmation visibility on guest details,
persistent acknowledgement errors, incomplete success-envelope ambiguity, and
result-write retries with task diagnostics. Regression tests cover the changed
response classification and desktop recovery states.

Local validation passed 217 non-desktop Rust tests, 150 desktop tests including
the screenshot comparisons, 40 lab unit tests, formatting, and workspace Clippy
with warnings denied. The Linux-only adapter and disposable real-PVE validation
recorded on November 2 were not rerun for this review; their source hashes remain
historical evidence, not attestation of these follow-up changes. CI now explicitly
tests and lints the optional Linux Proxmox feature and runs the lab safeguards.
