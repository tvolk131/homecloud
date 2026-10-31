# MVP decisions and implementation boundary

This records the decisions made after the draft architecture RFC, plus the
current implementation boundary. It is the implementation spec for the first host.

## Product

- One bare-metal x86 Linux host, Incus virtual machines, and local ZFS storage.
- Native macOS GUI using iced and iced-m3, with a shared Rust CLI/client library.
- Mac T3 Code client; development files, provider credentials and agents live in
  the owned Ubuntu guest. SSH reaches the guest through Iroh, with no public guest ports.
- Physical GPU passthrough is included. GPU sharing and time-based accounting are deferred.
- Single active SQLite controller. No additional consensus system or HA promise.
- No user thin provisioning, snapshots, shared CPU scheduling, or disk shrinking.
- Off-host backup is a follow-up explicitly deferred by the operator. Total host
  failure may lose all work. No recovery exercise has been claimed for this release.

## Identity and access

A stable user UUID owns resources; each device has an independent Ed25519 signing key.
Each GUI/CLI/SSH process creates a fresh Iroh transport endpoint. The device signs
a domain-separated TLS exporter bound to the connection and ALPN; the controller
verifies possession before handling enrollment, control requests or SSH routing.
This lets simultaneous processes authenticate as the same device without competing
for one relay endpoint registration. The server endpoint identity stays pinned.
Bootstrap and administrator recovery run through local privileged host access.
Invitations are random 256-bit secrets, hashed at rest, expiring and atomically
single-use. Revocation is permanent for that device key; recovery enrolls a fresh key.
Current authorization is checked on every control request and during active SSH
tunnels. Account disablement closes access without stopping the guest workload.

The server supplies only the immutable VM's private TCP/22 endpoint. OpenSSH
still authenticates the guest account. Host-key trust is enrolled from Incus's
guest agent during provisioning and pinned under `homecloud-VM_UUID`, independent
of ports and addresses. A changed key stops access and needs explicit inspection.
This trusts the physical host and Incus; it is not protection against a compromised host.

## Resource invariants

All admission, CPU/network/GPU leases and idempotency records commit in one SQLite
`BEGIN IMMEDIATE` transaction before an external effect. The first host uses a
reviewed resource budget. RAM changes use explicit capacity adoption; CPU topology
and storage-envelope changes still require an operator-reviewed migration.

The durable `Vm.resources` record is the RAM/disk commitment, not measured usage.
CPU rows uniquely claim every SMT sibling; GPU rows uniquely claim a PCI address
case-insensitively. Admission recomputes commitments under the SQLite write lock,
including requests whose host operations have not finished. A rejected request
rolls back all its claims, VM intent and operation together. Grant changes use the
same lock and count the larger of each user's grant and existing commitment.

Only one controller may apply host changes for a database: an OS file lock is held
for its lifetime, and reconciliation calls within it are serialized. Each apply
captures its operation, VM, inventory of managed VMs and current user grants in
one database transaction. This is a single-host constraint, not distributed locking.
The root host adapter also serializes physical changes. A busy/failed database
transaction rejects admission; it never falls back to unlocked allocation.

A late completion of an already successful operation is a no-op. It cannot release
claims held for a newer resize or detach. Unexpected lifecycle confirmations and
operations awaiting review retain their backing. Passive observations cannot mark
resources deleted; explicit deletion must complete. One unfinished operation per
VM is enforced both by admission and a unique database index.

Configured guest RAM counts while compute is claimed, including pending startup
and shutdown. A fully deprovisioned stopped VM holds disk only. Each CPU credit selects a
complete physical core, including all SMT siblings. QEMU starts through a static
pre-exec wrapper which checks a root-owned policy, moves into a pre-created VM
cgroup, verifies its effective cpuset, and restricts helper affinity before exec.
Housekeeping excludes all allocatable cores, even while they are unused. IRQ and
workqueue placement is best effort; kernel activity prevents a hard real-time guarantee.

Each user has a ZFS parent dataset above their Incus storage pool. The parent has
an equal quota and reservation covering the user's larger admission grant or
existing commitment. Incus child volumes are sparse internally, but aggregate
guest-visible capacity plus overhead cannot exceed that backing. This is not a
user-enabled thin-provisioning feature. User snapshots are absent.

The root pool also needs **unreserved accounting headroom**. The lab proved that
parent reservations alone fail when root `available` reaches zero; an emergency
reserve dataset alone does not solve it. The installed policy uses a conservative
30 GiB operating floor and a 128 GiB managed budget on the larger existing pool.
The 1 GiB floor that passed the 32 GiB scratch-pool experiment is not a universal
production sizing rule. Admission blocks further backing below the configured floor.
Out-of-band pool administration can violate these assumptions and needs operator review.

Quota reductions immediately block positive growth in each constrained dimension.
They do not stop guests or release existing backing. Resize holds the larger old/new
commitment until the host confirms completion. Deletion holds leases and backing
until Incus deletion is confirmed. GPU detach holds the lease until physical release.

Each physical GPU is exclusively leased while claimed. A fully stopped VM keeps
its preference but relinquishes ownership. Permission revocation blocks new
attachments and reacquisition, without interrupting a running attachment. Host display handoff is a local operator
policy. IOMMU evidence, group members, offered devices and next steps are visible.
Missing groups prompt firmware/kernel checks; they do not prove unsupported hardware.

GPU telemetry queries the existing Incus guest agent for `nvidia-smi` output.
No separate guest monitoring daemon is installed. Utilization, VRAM and temperature
are guest-reported, and unavailable telemetry is shown as unavailable. This is a
basic responsiveness check, not an independent fault detector.

`Vm.spec` is configured size; `Vm.resources` is charged backing. `preferred_gpu`
is remembered intent; `gpu` is a durable claim. A stop keeps CPU/RAM/GPU claims
until Incus confirms shutdown, GPU detach/driver handoff finishes, and the CPU
launcher policy is removed. Remaining VM host processes prevent release. The
adapter must explicitly return `compute_released=true`; an older adapter's
ordinary stopped-state response cannot authorize unallocation. One final database
transaction releases compute and completes the operation. Disk, network identity
and each user's host-backed grant remain held. A guest-initiated shutdown alone
does not release resources; explicit stop/cleanup is required.

Start atomically acquires RAM, complete physical cores and the preferred GPU.
A GPU conflict rolls back every new claim and leaves the VM stopped. The GUI
offers Go back or Start without GPU, which is a fresh request with current quota
and authorization checks. It records the CPU-only boot in the operation and
preserves the preference. Reconciliation and internal resize restarts use the
active claim, so they cannot silently reattach the remembered GPU. A later full
stop/start requests the preferred GPU again. Stopped resizes claim disk only;
running resizes retain the larger old/new backing through interruption.

Legacy GPU preferences are migrated additively. Old stopped VMs keep historical
claims until explicit cleanup confirms release; the GUI exposes Finish stopping
for that case. Ambiguous failures and operations needing review retain claims.
No expiration timer or observed idleness can release hardware ownership.

Database tests use independent connections and simultaneous contenders for each
resource, grant increases, resizes, GPU ownership across users, idempotent retries
and atomic stopped-VM start admission. Fault injection
checks rollback after claims are written; recovery and delayed completion tests
check retained backing. A subprocess verifies controller exclusion and lock release.
Host-adapter failure tests run through `cargo test` using Python 3 without root.
They verify cleanup order, shutdown confirmation, remaining host-process refusal,
GPU handoff failure, and CPU-only boot intent. These mocks do not replace real
Incus/GPU lifecycle testing on the Linux lab.

## Networking

Every VM has its own filtered Incus bridge/subnet. nftables blocks guest-originated
connections to the host, private/special-use ranges and other guest networks while
allowing bridge DNS/DHCP and outbound public IPv4. Unsolicited inbound and guest
IPv6 are blocked. There is no LAN proxy or arbitrary-destination tunnel API.

Public Internet services remain reachable, including any service that the operator
publishes publicly or through router hairpinning. The firewall cannot infer that
a public address belongs to a private service; add explicit denied destinations if
that becomes relevant. Public guest endpoints and private cross-VM networking are deferred.

## Operations and ownership

Operation phases are pending, applying, retrying, needs_review and succeeded.
UUID tags and fixed names make retries safe across an ambiguous Incus result.
Unknown ownership, missing recorded VMs, changed keys and unexpected configuration
require review. The controller never silently adopts foreign resources or recreates
a missing disk on a normal start request. `homecloud retry OPERATION_UUID` is an
audited administrator action after inspection; it does not bypass ownership checks.

Completed VMs are observed for lifecycle/configuration drift. A discrepancy is
surfaced while allocations remain held. Unexpected external stops are not silently
reversed. Physical backing reconciles independently of logical admission changes.

The network-facing daemon runs as `homecloud`, without Incus group membership.
A root-owned fixed-command adapter is invoked by a narrowly matching sudo rule.
Clients cannot supply arbitrary host commands, paths, image URLs or network destinations.
This adapter is Python for the first implementation; domain policy, identity,
persistence, transport, clients and reconciliation are Rust. The CPU launcher is C.

VM resource metrics use a separate unprivileged Rust HTTPS client to Incus's
loopback metrics-only listener, with a pinned server certificate and a dedicated
metrics-only client identity. The installer provisions this access as root once;
runtime sampling does not pass through the privileged adapter. Incus's existing
agent supplies guest usage, including actual root-filesystem space. See
[VM metrics](metrics.md) for setup, scope and measurement semantics.

## Current roadmap

The live backlog is maintained in [roadmap.md](roadmap.md). The dated
[validation record](validation.md) distinguishes completed proofs, resolved findings
and deliberately deferred acceptance work. Older lab evidence is historical.


T3 is the first [managed guest application](managed-apps.md), selected per VM.
Installation and account setup have independent durable operations; they do not
hold the VM resource-operation lane. The Mac uses native T3 Connect account
discovery for the backend, while ordinary SSH continues through Iroh.


## Admin host reserve editing

Administrators edit total host reserves in Hosts → host → Overview → Edit reserves.
The root-reviewed profile exposes explicit minimum memory, complete physical CPU
cores and pool free-space floor. The VM envelope is physical managed capacity
minus those minimums; the durable reserve record stores the amount above each
minimum. The GUI and public request use totals. Lowering the former conservative
lab reserve therefore releases real unissued capacity. Unmanaged workloads must
be separately budgeted or retired before expanding that envelope.

Admission protects the sum of each user's component-wise `max(grant, committed)`,
including disabled users, stopped VM disks and operations still cleaning up.
Unused grants remain promises. Stopping a VM returns compute to its owner's unused
grant. Reserve increases use only unpromised capacity; a decrease does not
implicitly issue a larger user grant. Complete CPU groups are moved only when
unclaimed, and existing running guests keep their vCPU pins.

Total system memory reserves contain a reviewed operating floor for Linux,
services and VM overhead, plus an ARC allowance. The allowance equals total system
reserve minus that floor. ARC occupancy is never charged twice, and guest grants
never change based on RSS. The lab's 4-GiB system minimum contains a 2-GiB
operating floor and 2-GiB cache allowance, with one complete housekeeping core as
its CPU minimum. Other hardware and VM-density profiles require validation.

Admin cache modes are Automatic (Homecloud) and Custom limit. Automatic chooses
the root-reviewed per-host recommendation within the protected allowance. Both
modes persist a positive numeric cap and verify the effective `c_max`. Automatic
never restores ZFS's native zero/default or borrows tenant promises. Native-default
calculations remain read-only telemetry. Actual cache size can temporarily exceed
its cap; a cap-only reduction can complete, but releasing system reserve waits for
actual reclamation before that capacity becomes available for new promises.

The helper rejects conflicting external modprobe settings, journals intent,
rebuilds initramfs and verifies enforcement. A temporary, journaled
`zfs_arc_sys_free` target asks ARC to reclaim before reserve release and is restored
on completion or recovery. A timeout leaves Applying with capacity held. Swap
remains emergency fallback; this is not a promise of zero paging under arbitrary
workloads. The earlier native-Automatic experiments are historical evidence in
`lab/evidence/host-memory-policy.json`; the bounded policy and recovery proofs
supersede their configuration recommendations.

SQLite serializes grants, VM claims and reserve intent. Pending changes retain
the stricter old/target capacity and union of reserved core groups until root
confirmation. The private fsynced journal precedes effects and is replayed at
boot. Unknown/mismatched policy fails admission closed. Recovery retains claims;
only a provably unapplied operation can be durably discarded. The pool floor
remains operating headroom, not a ZFS reservation or protection from foreign writes.

Fleet inventory and stable host identifiers must precede editing another host's
reserves from a shared controller; the current directory lists the connected host.


### Memory-policy reliability follow-up

The implementation now uses one cloud-init seed transport for each pinned image,
a durable launch fence across the image's first-boot reboot, bounded agent/setup
waits, cancellable incomplete creations, and an explicit system-backed ARC budget.
The lab harness retains manifests and claims on failures. See the current
reliability evidence in [validation](validation.md) for actual fault cases,
limitations and remaining upstream compatibility risks.
