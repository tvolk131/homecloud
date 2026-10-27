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

A stable user UUID owns resources; each device has an independent Iroh key.
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
fixed, reviewed resource budget; changing its total capacity needs an explicit migration.

Configured guest RAM counts, including stopped VMs. Each CPU credit selects a
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

Each physical GPU is exclusively leased. A stopped VM retains it; permission
revocation only blocks new attachments. Host display handoff is a local operator
policy. IOMMU evidence, group members, offered devices and next steps are visible.
Missing groups prompt firmware/kernel checks; they do not prove unsupported hardware.

GPU telemetry queries the existing Incus guest agent for `nvidia-smi` output.
No separate guest monitoring daemon is installed. Utilization, VRAM and temperature
are guest-reported, and unavailable telemetry is shown as unavailable. This is a
basic responsiveness check, not an independent fault detector.

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

## Next work before coworker release

- Verify a real T3 provider session survives disconnect/reconnect; the pinned
  background service is provisioned, while provider login belongs to the guest's owner.
- Exercise off-LAN Iroh transport and Mac sleep/network-change recovery.
- Add operation cancellation/recovery UX, better forms/navigation, activity history,
  persistent alerts, SMART/disk inventory, and idle-GPU advisories.
- Harden the host installer with package/version checks, migration/rollback paths,
  broader hardware discovery and explicit upgrade behavior. The sample policy is lab-specific.
- Store Mac device secrets in Keychain. Current private files use mode 0600.
- Broaden crash/fault injection, hostile guest/network cases, and device reset/reboot tests.
- Design off-host backup/restore, schema migration and image-update policy before
  treating the system as durable storage for important work.
