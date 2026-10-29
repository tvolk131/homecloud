# Lab operation and recovery

The current lab host is `tommy@10.0.0.44` (`resolvr-box`), Ubuntu Server 26.04.1, Incus 6.0.5,
OpenZFS 2.4.1, an i7-8700K, 48 GiB installed RAM (about 45.98 GiB Linux-usable), and a GTX 1070 Ti.
Its existing `hclab` pool is a disposable HDD; the Ubuntu NVMe was retained.
No disk formatting occurs in the Homecloud installer.

The host moved from `192.168.86.204` on the Google router to the Comcast LAN. Its
SSH host key and Iroh endpoint identity were verified unchanged. DHCP addresses
may change; pin the machine identity rather than trusting a newly discovered IP.
The host currently runs kernel `7.0.0-38-generic`; the Ubuntu 24.04 guests run 6.8.
The controller starts at boot without a console login.

## Installation boundary

The host must already have a healthy ZFS pool, working Incus/QEMU, nftables,
`br_netfilter`, PCI tools, a C compiler with static libc, and the pinned Ubuntu
cloud image cached in Incus. Review the hardware policy locally before installing:

```sh
cargo build -p homecloud-daemon -p homecloud-cli
sudo python3 scripts/install_host.py \
  --policy examples/lab-host.json --daemon target/debug/homecloudd
```

**The example is only for this lab.** It allocates cores 4/10 and 5/11 to new VMs,
reserves 0/1/6/7 for housekeeping, and reserves 2/8 and 3/9 for the earlier lab VMs.
Those legacy VMs are currently stopped with autostart disabled.
That checked-in example predates the live hardware/profile changes. The current
installed profile has a 41 GiB VM memory envelope and a 4 GiB minimum system
reserve, including a 2 GiB operating floor and a 2 GiB ARC allowance. Existing user
RAM grants remain 4 GiB. Review the installed profile and journals before changing
or reinstalling policy; a fresh machine needs its own topology and capacity file.

Installation changes system/user slice CPU affinity, Incus's QEMU launcher path,
systemd units, a dedicated nftables table and the restricted helper sudo rule.
The explicitly listed disposable legacy VMs may need a graceful restart for
AppArmor changes. Installer backups live under `/var/lib/homecloud/install-backups`;
they are local configuration copies, not an off-host recovery strategy.

Installed files:

- `/etc/homecloud/host.json`: root-owned hardware policy.
- `/etc/homecloud/control.json`: controller capacity and helper location, owned by
  `root:homecloud` with mode `0640` so the service account can read it.
- `/etc/homecloud/cpu-policy.tsv`: generated launch allocations.
- `/var/lib/homecloud/control`: private SQLite database and Iroh endpoint secret.
- `/var/lib/homecloud/host`: root-owned CPU/GPU handoff state.
- `/usr/local/libexec/homecloud`: fixed host adapter and QEMU wrapper.
- `/var/log/homecloud-cpu-launch.jsonl`: pre-exec CPU fence evidence.

## Bootstrap and pairing

Run on the host through local privileged authority:

```sh
sudo -u homecloud homecloudd --config /etc/homecloud/control.json \
  bootstrap-admin --name Tommy
```

The JSON output includes a secret single-use pairing code, valid for 20 minutes.
Do not paste it into logs, issues or screenshots. Transfer the public
`/var/lib/homecloud/control/server.json` through the trusted host connection and
verify its endpoint identity, then pair in the Mac app or CLI:

```sh
homecloud enroll --ticket server.json
```

The CLI reads the code from stdin. The current Mac has already been paired.
Bootstrap is permanently closed while an administrator record exists.

## Inspect and repair

```sh
systemctl status homecloudd incus homecloud-host-layout
sudo journalctl -u homecloudd
homecloud status
sudo zpool status hclab
```

An unfinished operation keeps its allocations. Inspect the VM and error before
retrying; do not fix ambiguity by deleting unrelated Incus resources. A `needs_review`
operation can be retried by an administrator with `homecloud retry OPERATION_UUID`.
There is currently no force-stop or automatic destructive recovery API.

For a lost administrator device, use local privilege and the existing user UUID:

```sh
sudo -u homecloud homecloudd --config /etc/homecloud/control.json \
  recover-admin USER_UUID
```

Enroll a fresh device key with that new code. Previously revoked keys stay revoked.
Endpoint-key loss and total database loss are not equivalent to device loss; stop
and inspect before reinitializing either. A replacement endpoint must be verified
and paired explicitly. Backup/restore remains deferred.

## GPU and SSH

GPU attachment may blank the host display. The lab operator explicitly chose SSH
management during handoff. The saved original drivers and console state are restored
on detach. New hardware needs verified IOMMU groups and a supported guest driver;
the lab's Pascal GPU uses the R580 branch with Ubuntu-signed kernel modules.
The saved GPU preference is separate from ownership. A completed stop releases
CPU/RAM/GPU claims after physical cleanup; a later start requests the preference
again. If it is occupied, the GUI offers Go back or Start without GPUs. This explicit fallback omits all remembered GPUs for that boot; it never silently selects a partial set. Internal
resize restarts retain their current claims.

Direct DKMS installation requires MOK enrollment and is not the default unattended
path. Guest Secure Boot remains enabled.

OpenSSH trust files are generated beside the selected client profile under `ssh/`.
They use immutable VM UUID host aliases. Never bypass a changed-key warning with
`StrictHostKeyChecking=no`; inspect the guest/reimage history first.

The earlier temporary lab bootstrap sudo rule is still present because the proving
and installation work uses privileged host access. Remove it before a coworker
release after establishing the permanent operator workflow; the daemon's own
sudo rule only permits the fixed adapter command.


## Lab validation and upgrade notes

The October 27 matched controller/adapter upgrade preserved the database, endpoint
identity, device pairing and existing devbox. Live proofs covered competing GPU
requests, cleanup failure, controller crash replay, disk backing after lowered
quota, network isolation and unattended host reboot; see [validation](validation.md).
The two-owner test temporarily used an 8 GiB managed RAM budget with legacy VMs
stopped. Cleanup restores the reviewed 4 GiB budget and leaves the existing devbox
running CPU-only with its GPU preference retained. Memory capacity changes use the reviewed adoption workflow below. CPU topology and storage-envelope changes still require an explicit local migration; changing only their policy files is refused at startup.

T3-enabled guests install `libatomic1` alongside the minimal image's standard
packages. Additional development tools remain owner-installed through apt.
The guest's T3 service may briefly report active before its loopback socket opens;
connection readiness must check the listener as well.


Transport protocol v2 separates the stable enrolled device key from each process's
Iroh endpoint. Existing pairings and the database are preserved, but the daemon,
CLI and bundled desktop client must be upgraded together. Older clients negotiate
a different ALPN and cannot use the new service. The SSH configuration must point
to the updated CLI; run `homecloud ssh-config VM_UUID --install`.
No firewall exception or public port is needed for hotspot access.


For connection diagnostics, `homecloud --relay-only status` disables direct IP
transports. `homecloud --relay-only ssh-config VM_UUID --install` also places the
option in that VM's ProxyCommand; exporting normally removes it again. Host-key
verification and device authorization stay enabled. `RUST_LOG=homecloud_client=debug`
writes path metadata to stderr. Do not capture invitation codes or credentials in
diagnostic artifacts. Normal networking remains the default. The lab Mac's devbox
alias currently uses relay-only mode for the provider-session check.


Managed application setup now uses a separate application worker and supervised
guest jobs. See [managed applications](managed-apps.md) for T3 account connection,
recovery, and the distinction between Homecloud access and T3 Connect access.
The installer deploys `managed_apps.py`, `managed-app.py`, and `setup-t3.sh` next to
the fixed host helper; keep those files and the daemon at a matching version.


## Graceful and forced shutdown

The ordinary Stop action requests a graceful guest shutdown with a 30-second
Incus timeout. A failed stop, including an adapter timeout, moves to manual review
with all resource claims held. It does not retry indefinitely or escalate to force.
Use Keep waiting in the workspace to submit another graceful attempt, or Force
stop… to confirm an immediate power-off. The equivalent explicit CLI action is
`homecloud stop <vm-id> --force`; ordinary `stop` remains graceful.

Owners may stop their own VMs; administrators may stop any managed VM. Forced
shutdown is audited separately as `request_force_stop`. It can discard unsaved
work and in-flight writes. It retains the disk and remembered GPU preference.
It cannot fix a hung host, hypervisor, or GPU reset/cleanup failure.

Only a no-longer-executing stop in `needs_review` can be superseded by a new stop.
The previous operation becomes terminal with its original error retained in the
record. An active apply or an unrelated reviewed operation blocks the request.
The single transaction preserves claims and the unique active-operation index;
late completion/phase updates from the superseded request are refused. A force
failure also requires explicit review rather than repeated automatic power-offs.
Restart recovery resumes an already-authorized durable intent, observing an
already-stopped guest before attempting any further power-off.

Neither a successful Incus command nor an observed stopped state releases compute
by itself. The adapter must also finish GPU detach/handoff and CPU-policy cleanup.
If the guest is off and cleanup fails, retry Finish stopping; inspect the host if
that continues to fail. Never clear leases manually to make the UI look available.


## Unified workspace edits

`Mutation::Edit` carries a `WorkspaceEdit` (name, memory_gib, cores, disk_gib,
gpu) and an explicit restart_confirmed flag. GPU edit is tagged Keep or SetMany
with a list of PCI addresses; an empty list detaches/forgets every card. Keep
preserves active assignments and next-start preferences independently. The old
single-device Set and Gpu mutation remain readable for old operations, but reject
multi-GPU configurations instead of silently truncating their selection. The CLI exposes the same
operation, for example `homecloud edit VM_ID --name new-name`, or resource changes
with `--memory 8 --gpu PCI_A --gpu PCI_B --restart` (comma-separated lists also work). Omitted GPU flags mean Keep;
`--no-gpu` clears the assignment/preference. Running resource changes require
explicit --restart, checked by both CLI and server.

The store authorizes owner/admin, serializes the VM operation lane, validates
restart consent against current state, and admits all resources in one SQLite
transaction. A failed admission changes neither name, specification nor claims.
Rename-only edits complete locally and are audited without calling the host.
Stopped preference-only edits do likewise. Stopped size edits hold disk backing
only and apply without starting the guest; compute/GPU admission happens on Start.

`Vm.gpus` holds active/retained claims, `preferred_gpus` holds the next-start set,
and `pending_gpus` holds additions during an edit. Old scalar records upgrade on
read; serialized collection fields are authoritative. The existing GPU lease table
permits multiple PCI claims per VM, with global case-insensitive PCI uniqueness.
CPU/RAM/disk backing and every incoming GPU are claimed in the same SQLite commit.
A competing start/edit claims its whole set or nothing. Stopped edits claim no GPUs;
a later start rechecks quota, permission, CPU placement and all GPU conflicts.

Old and new GPU claims remain held through physical cleanup and manual review,
including controller restart. Completion retains exactly the target set and releases
only removed cards. A full stop releases all GPUs only after every device is
detached/restored and CPU cleanup succeeds. Saved preferences remain independent.
Do not downgrade to a one-GPU binary after creating multi-GPU records or operations;
scalar compatibility mirrors alone cannot preserve multiple attachments.

The host validates restart consent and all incoming claims before changing a
running instance, gracefully stops for required changes, applies sizes and GPU devices,
and starts the guest. Replay observes already-applied differences without another
restart. A shutdown timeout never forces power off. Driver/provisioning or SSH
identity failures retain claims for operator inspection and retry; physical host
changes are not an all-or-nothing rollback transaction.

Do not power-cycle the active devbox just to validate this feature. The lab's
configured RAM is fully committed. A live combined resize/GPU replacement proof
requires a disposable running VM or coordinated owner downtime. Rename-only and
stopped-VM edits can be checked without interrupting the active guest.


Multiple physical GPUs use separate Incus `gpuN` devices. Retained cards keep their
device names; additions use free slots. Every incoming card is preflighted before
a resource edit shuts down a running VM. Unrecognized, inherited, duplicate or
unclaimed GPU devices block reconciliation for review. This does not bypass unsafe
IOMMU groups or add vGPU/MIG sharing. Guest driver setup retains the existing
NVIDIA support; multi-GPU readiness also requires every selected card to appear in
the supported guest query. Per-device utilization is explicitly unavailable for
multi-GPU guests until guest/host card identities can be matched safely.

The current lab contains one physical GPU. Automated set-lifecycle tests cover
multi-card admission and fault recovery; simultaneous physical multi-card
passthrough still requires a host with at least two independently assignable GPUs.


GPU attachment does not impose a separate guest-RAM minimum. The normal VM size
validation (at least 1 GiB) applies to create, resize, edit and start. The former
hardcoded 4 GiB GPU policy had no documented hardware/driver proof and was removed.
Memory recommendations for managed applications remain separate; the T3/Claude
devbox recommendation is still 4 GiB. Smaller GPU guests remain subject to actual
image, driver and workload needs; 1 GiB admission is not a hardware acceptance claim.


## Host reserve policy and recovery

Use admin Hosts → host → Overview → Edit reserves, or:

```sh
homecloud reserve-set --memory 4 --cores 2 --disk 30 --arc-auto
homecloud reserve-set --memory 4 --cores 2 --disk 30 --arc-limit 2
homecloud reserve-retry <operation-id>
homecloud reserve-discard <operation-id>
```

Memory/disk and custom cache limits use whole GiB. Reserve arguments are **totals**;
CPU counts complete physical cores. `--arc-auto` and `--arc-limit` conflict. Omitting
both preserves the previous cache mode. Requests carry expected revisions and
idempotency keys (`--expected-revision`, `--request-id`). Poll `homecloud status`
for verified completion rather than treating an accepted operation as applied.

The root-owned host profile defines minimum reserves and the initial VM envelope; controller `meta.limits` must match that installation baseline. Reviewed RAM changes use a separate durable capacity journal and leave the baseline files unchanged. Ordinary reserve edits vary the amount above reviewed
minimums. Existing unmanaged workloads need explicit accounting or retirement.

`/var/lib/homecloud/host/resource-reserves.json` is the private fsynced root journal;
`meta.host_reserves` and `host_reserve_operations` hold controller intent/completion.
The root adapter derives CPU policy and managed systemd drop-ins. Boot CPU-layout
replays them before Incus starts. Running guest vCPU pins are preserved.

The ARC maximum is host-wide and elastic within its protected system allowance;
it is not a second reservation. `/etc/modprobe.d/zz-homecloud-arc.conf` contains
managed boot policy. Conflicting external settings require explicit local adoption;
the network API cannot edit arbitrary files. Both Automatic (Homecloud) and Custom
persist numeric caps. The adapter rebuilds initramfs and verifies effective
`c_max`. A cap-only change can complete while occupancy shrinks, but releasing
system RAM waits for actual cache reclamation. Cache size never substitutes for
configured guest-memory accounting. See the reviewed-profile contract below.

An ambiguous failure keeps capacity fenced. Retry replays the same operation;
already-applied root proof permits completion without repeating host effects.
Never clear a pending row or delete the root journal to recover capacity. Discard
requires the applied before-state and a tombstone under the host-operation lock.
Partial/applied changes must finish recovery before a new policy can release capacity.

Future consistent backups must include both host configs, controller DB/secrets,
the complete private host state (reserve/CPU journals, startup markers and
delete fences) and managed ARC boot configuration. Restoring only the DB
can disagree with live policy and pauses admission. Off-host backup remains deferred.


### Reviewed memory and cloud-init profiles

The root-owned host profile sets `memory_operating_floor_bytes` and
`arc_recommended_maximum_bytes`. Both must fit within the installed minimum total
memory reserve. ARC's allowance is total system reserve minus the operating floor;
its cap is never counted a second time. Homecloud Automatic chooses the reviewed
recommendation within that allowance and persists a positive numeric ceiling.
It does not restore ZFS's native zero/default or grow from unused tenant promises.
The lab's 4 GiB system minimum includes a 2 GiB operating floor and a 2 GiB cache
allowance. This is a tested host profile, not a universal hardware minimum.
Swap remains an emergency fallback; no policy promises zero paging for arbitrary
kernel, VM and I/O workloads. Do not automatically tune swappiness from guest RSS.

A reduction in system reserve keeps database capacity fenced until actual cache
reclamation is confirmed. A private journal restores any temporary
`zfs_arc_sys_free` target after interruption. The target asks ZFS to reclaim; it
never creates artificial allocation pressure. If reclamation cannot complete,
leave the operation Applying and investigate rather than issuing that RAM again.

Choose one cloud-init seed transport for the pinned image. The supported
`images:ubuntu/24.04/cloud` image uses `cloud_init_transport: "agent"` (default).
Its agent applies templates then reboots once. Do not add a NoCloud config disk to
that image: it races the agent and can interrupt network configuration. Only a
reviewed disk-only image without that template path may use `"disk"`. Existing
VMs are not automatically converted or reimaged.

First-boot waits are bounded and survive controller restarts. A prior launch,
active Incus operation, live QEMU process, or VSOCK mismatch never authorizes a
second blind start. Claims remain held when review is required. A failed creation
can be explicitly deleted, including before pairing or SSH provisioning completes;
the host records a deletion fence so a delayed create cannot resurrect its ID.
For package failures, inspect `/var/log/cloud-init-output.log` and guest networking.
APT uses three acquisition retries and forces IPv4, matching the V1 guest network.
Retries do not erase cloud-init history or reimage disks. Repair the guest explicitly
or delete/recreate an unused failed VM through the normal API.

Repeating Start while the VM already has Running intent and compute claims records
a completed no-op, including during a guest-initiated reboot. It does not enqueue
another Incus start, reacquire resources, or clear an existing error. Normal Start
from a fully stopped VM still admits and provisions resources. Use reviewed retry
after repair for a failed operation; repeating Start is not a repair action.
For an unexpectedly stopped VM whose Running intent still holds compute, inspect
the cause, then explicitly Stop to finish cleanup before starting it again.

An unchanged legacy Resize and detaching an already-unattached GPU also complete
locally. Stopped-VM GPU changes update the remembered preference only. These
requests keep observed state, errors and claims intact; they do not dispatch an
Incus start or stop. The newer Edit path already treats metadata-only changes
locally. A real resource change still uses the physical operation lane.

The host's durable launch marker survives successful guest setup and the host's
response to the controller. This prevents replay after a lost acknowledgement from
issuing another start during an Incus-internal reboot gap. Confirmed shutdown
clears the marker before a genuinely new start; verified deletion cleans it up.
Ambiguous startup remains bounded and requires review with claims held. This does
not make external Incus transitions atomic with controller state.

There is no global button cooldown: different, legitimate changes may execute
sequentially after their predecessors finish. While an operation is unfinished,
fresh conflicting VM mutations are rejected rather than queued. The exceptions
are explicit cancellation of creation and replacement of a reviewed stop by an
explicit shutdown request; both retain claims until cleanup is verified. Replaying
the same idempotency key returns its original operation and cannot change payload.

The VSOCK configuration check compares Incus's stored CID with its generated QEMU
file. Agreement does not prove the running guest uses that CID: an overlapping
failed start can rewrite both while an earlier process remains alive. If the agent
stays unavailable, inspect the actual process generation and, when enrolled SSH is
available, the guest kernel's local CID. Plan any stop/start explicitly; never free
claims or kill processes solely because the metadata agrees or the agent times out.

### Inspecting incomplete VM setup

The VM details page shows the last observed setup stage and any pending or reviewed
failure. CPU/RAM/GPU claims remain held until verified shutdown and cleanup; disk
backing remains held until deletion. Stages are observations, not percentage or
completion-time estimates. An unfinished operation remains visible even after the
recent completed activity history fills up.

**View setup details** fetches recent guest cloud-init output on demand. The owner
or an administrator can read it; other users and revoked devices cannot. Each log
is limited to its last 12,000 bytes and the guest-agent request has an eight-second
limit. These logs are not stored in the controller database or included in ordinary
status polling. If the guest agent is unavailable, the dialog says so and can be
refreshed. The CLI equivalent is `homecloud diagnostics <VM-ID>`.

**Retry after repair** is an explicit administrator action. It rechecks the existing
instance and resumes safe Homecloud work; it does not reset the disk or rerun failed
cloud-init package/script stages. A new VM whose setup failed may not yet have an
SSH identity enrolled, so repairing it can require the operator's local/SSH access
to the host and Incus guest execution. Alternatively, **Delete incomplete VM…**
opens the normal deletion confirmation. Deletion keeps all claims until physical
cleanup is confirmed. An Incus/QEMU disagreement requires operator inspection;
Homecloud never interprets a timeout as permission to kill a process or free claims.


## Desktop Create recovery

The desktop saves each submitted Create's exact configuration and request key
before transmitting it. The private sidecar is `<paired-profile>.create-request.json`
(default `~/.config/homecloud/client.json.create-request.json`); it contains public
host/device IDs and the guest SSH public key, not credentials or private keys.
The journal is written atomically and synced before sending. A per-profile file
lock allows one desktop instance to manage creation recovery at a time.

A timeout means the outcome is unknown. The confirmation dialog shows the saved
configuration and **Retry request**; it never changes that submission's values or
key. Closing the dialog does not cancel the request. New VM reopens its recovery
view until resolved. Retry bypasses current GUI capacity checks because the
original accepted allocation may already consume the entire quota. Reopening the
app automatically replays its saved submission using the same paired identity.
Corrupt or mismatched journals block new creation and remain intact for recovery.

The `CreateVm` RPC returns the original Operation when accepted. Admission
rejections are persisted in `create_rejections` before returning `CreateRejected`.
The rejection transaction rolls back partial VM/network/CPU/application claims
before committing the receipt. A delayed copy of that key remains rejected after
quota changes, including through the legacy mutation API. A different payload
cannot reuse a resolved or rejected key. Authentication, transport, reserve-policy,
commit and unexpected-response errors do not clear the client's unresolved record.

After acceptance, Homecloud clears the local record and follows the original
operation in Activity. A committed rejection clears the record and permits editing
and a new submission with a new key. Failed local receipt cleanup retains recovery;
retry cannot allocate another VM. Keep server rejection receipts, just like accepted
operation keys, while delayed clients may replay requests. Deploy the matched
controller and desktop together; do not delete unresolved journals to work around
a timeout or downgrade the controller past these receipt semantics.


## Physical RAM changes

Hardware discovery never issues added RAM automatically. The adapter reports
usable physical RAM, the operating/cache minimum, the reviewed VM envelope and
the currently safe envelope. Admission immediately uses the smaller reviewed or
physical envelope, less active/pending system reserves. Grants and held claims
remain durable even if that produces a deficit. Per-dimension neutral/reducing
operations stay available; no new memory claim is admitted against a deficit.

Admins use **Hosts → System reserves → Review detected RAM change**. Review the
new VM envelope, promises and held memory, then adopt it. CLI equivalent:

```sh
homecloud memory-adopt --memory NEW_VM_ENVELOPE_GIB --expected-revision REVISION \
  --request-id REQUEST_UUID
```

The controller records intent/audit first. The fixed root helper checks fresh
physical RAM and ARC enforcement, waits for required reclamation, then fsyncs
`/var/lib/homecloud/host/memory-capacity.json`. SQLite records the attested revision
and completion. Lost replies replay the same operation; no config or SQL editing
is needed. Pending decreases fence admission immediately. Retry uses the same
request; cancelling an unapplied update requires a root-side durable tombstone
before SQLite releases that fence (`homecloud memory-cancel OPERATION_UUID`).
Applied capacity cannot be undone by cancelling a lost acknowledgement.

Managed VMs use `boot.autostart=false`. After each kernel boot, Homecloud reasserts
already-held Running intent once, only when held RAM fits and ARC is enforced.
A daemon restart or an Incus guest-reboot gap does not enqueue another boot.
When held memory exceeds physical capacity, keep the guests off, explicitly stop
them to confirm/release compute, resize them while stopped, and correct grants
before starting. A running resize is rejected before intent when it would need
this recovery path. The management service wants Incus but does not require it or order itself after Incus startup;
ARC failures block workloads while leaving recovery APIs reachable. CPU/network
safety fences remain mandatory. Read-only hardware inventory reports Incus/ZFS
inspection failures separately. Incus reads are bounded to eight seconds, and failed observations mark VM state unavailable while retaining all claims.

ZFS's default minimum may rise after a RAM increase. Homecloud accepts it if it
fits within the backed cap; the old separate `arc_minimum_limit_bytes` threshold
is obsolete and ignored. If a smaller requested cap conflicts with the effective
minimum, persist coordinated numeric bounds, lower min before max, and verify
ARC stats. This never restores ZFS's large native auto cap. Cache occupancy must
still yield before reserved capacity is released.

### Historical October 29 manual upgrade

The October 29 upgrade installed 16 + 8 + 16 + 8 GiB DDR4 DIMMs. All four are
reported by firmware; Linux exposes about 45.98 GiB after kernel/platform
reservations (including a 1 GiB crash-kernel reservation). The reviewed guest
memory envelope increased offline from 11 to 41 GiB. The 4 GiB minimum system
reserve, 2 GiB ARC cap, existing user grants, VM sizes and resource claims remain
unchanged. Remaining fractional memory is outside the whole-GiB managed envelope.

ZFS's effective default ARC minimum increased to 1,542,968,320 bytes after the
upgrade. This exceeded the old profile's 1 GiB `arc_minimum_limit_bytes` guard and
blocked the old combined safety-policy unit, preventing Incus/controller startup. That redundant guard and service coupling have since been removed.
The reviewed threshold is now 2 GiB, within the existing cache allowance; the
actual minimum is still strictly below the 2 GiB cap. No ARC kernel parameter,
boot configuration or system-reserve amount was changed to bypass that check.

That earlier drill used an offline migration before the adoption workflow existed:
back up both configs and database, fence controller and host-operation writers,
verify physical memory minus the protected floor, update host/control configs and
`meta.limits` consistently, and preserve grants/claims. A partial mismatch remains
fail-closed. Restore the host-layout policy before Incus/controller, then verify
inventory, the VM budget, guest agent and managed application listener. This
upgrade's local backup is under
`/var/lib/homecloud/test-runs/ram-upgrade-20261029`.
