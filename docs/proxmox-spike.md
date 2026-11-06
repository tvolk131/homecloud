# Proxmox-backed Homecloud research spike

## Question and boundary

Can Homecloud become a small-team development-environment layer over Proxmox,
retaining managed T3 setup and hard tenant disk boundaries while delegating VM
execution, templates, and backup/restore to Proxmox?

This is a substrate experiment, not a migrated Homecloud backend or finished GUI.
The experiment uses a disposable nested Proxmox server on `resolvr-box`. It does
not reinstall the physical Ubuntu host, format a physical disk, import its real
pool into Proxmox, copy existing guest credentials, or use an existing coworker's
VM as a test fixture.

## Safety and topology

- A private local manifest records exact resource identities and baseline VM
  states before mutation. Resources carry the run UUID; cleanup rejects foreign
  identities and tags.
- The outer ZFS parent has a 128 GiB quota and reservation. Its Incus worker has
  12 GiB RAM, four shared vCPUs, a 32 GiB root disk, and a 64 GiB scratch disk.
- The temporary CPU-launcher entry shares only the installed housekeeping CPUs.
  Existing VM assignments are unchanged; the original policy is backed up and
  checked before restoration. This is not a CPU performance experiment.
- Homecloud's physical-host controller is temporarily stopped to prevent new
  allocations while the out-of-band worker exists. Existing VMs may keep running.
- The worker's private NAT bridge inherits the existing `hcvm*` firewall rules.
  Proxmox's API is exercised from worker loopback, not exposed publicly.
- Inside Proxmox, only the serial- and size-verified scratch disk may be formatted.
  Separate ZFS parents back the base template, Alice, and Bob. Tenant storage
  entries use non-sparse ZFS volumes.
- API tokens and generated guest SSH keys stay in the disposable worker. They
  are not printed, committed, or reused from the host. Tokens expire automatically.
- Cleanup deletes only tagged fixtures, restores the original CPU policy,
  restarts the original controller, and checks baseline workload states.

## Reproduction

The SSH configuration is the existing pinned management connection. Keep the
manifest and raw observations private, outside tracked evidence.

```sh
python3 lab/proxmox_spike.py prepare --state .local/proxmox-spike/state.json
python3 lab/proxmox_spike.py install --state .local/proxmox-spike/state.json
python3 lab/proxmox_spike.py reboot --state .local/proxmox-spike/state.json
python3 lab/proxmox_spike.py probe --state .local/proxmox-spike/state.json
python3 lab/proxmox_spike.py storage --state .local/proxmox-spike/state.json
python3 lab/proxmox_spike.py templates --state .local/proxmox-spike/state.json
python3 lab/proxmox_spike.py t3 --state .local/proxmox-spike/state.json
python3 lab/proxmox_spike.py cleanup --state .local/proxmox-spike/state.json
```

Run one action at a time. Failed actions retain their manifest and fixtures for
inspection. Cleanup remains explicit; a failed guest operation is not permission
to delete unrelated storage or force-stop an existing VM.

Use a new manifest for a new experiment; a cleaned manifest cannot be reused.
An unfinished minimal Debian worker may lack the services needed to handle an
ACPI shutdown. If normal cleanup times out, the explicit
`cleanup --force-worker-stop` option permits a force-stop of **only** the
manifest-owned, tag-verified disposable worker, after another graceful attempt.
It never selects an original Homecloud VM. Cleanup also checks controller health,
the restored CPU policy, absence of lab resources, and physical-pool health.

For an unattended sequence, arrange cleanup even when an action fails:

```sh
set -e
trap 'python3 lab/proxmox_spike.py cleanup --state .local/proxmox-spike/new-run.json' EXIT
for action in prepare install reboot probe storage templates t3; do
  python3 lab/proxmox_spike.py "$action" --state .local/proxmox-spike/new-run.json
done
```

If the cleanup trap fails, inspect the error and finish cleanup before declaring
the host restored. Do not run another lab concurrently or change grants through
another client while the physical controller is stopped.

The installer uses the official Proxmox signed Debian repository. The inner
Debian cloud image is checked against the publisher's SHA-512 checksum. Cloud
image networking and root filesystem expansion are explicit: the Incus Debian
image initially has cloud-init disabled and does not grow its own filesystem.

Offline safety tests:

```sh
PYTHONPYCACHEPREFIX=/private/tmp/homecloud-proxmox-pycache \
  python3 -m unittest discover -s lab -p test_proxmox_spike.py -v
```

## Acceptance cases

1. Boot the Proxmox kernel, its management services, and two KVM guests.
2. Provision full clones into separate tenant storage through the API.
3. Prove host-enforced quota failure and continued writes for the other tenant
   with Homecloud's controller offline.
4. Prove snapshots consume the bounded tenant hierarchy and cannot grow forever.
5. Reject an oversized virtual-disk resize without breaking either guest's writes.
6. Check reserved volume backing, logical disk size, actual tenant space, and
   CPU/RAM telemetry separately.
7. Verify an Alice-scoped token cannot inspect Bob or administer host storage.
8. Run the real T3 boot service and an independent persistent workload; check
   transport disconnection, guest reboot, backup, and restore into the same tenant.
9. Remove the lab and restore the physical host's original control/workload state.

## Interpretation limits

- ZFS dataset provisioning and properties use a root helper inside the disposable
  Proxmox server. This experiment does not establish an API-only implementation.
- A quota is not a reservation, and virtual disk capacity is not physical
  consumption. Non-sparse volumes and operating headroom must remain part of the
  policy, rather than treating host out-of-space errors as ordinary guest limits.
- Full clones avoid assuming that linked clones can cross tenant storage entries.
- T3 Connect authorization, personal provider authorization, and an actual model
  task require separate human-authorized acceptance. A running T3 service and a
  persistent test process do not establish that those flows work end-to-end.
- Backups in this experiment stay inside the disposable worker. Restore testing
  does not establish off-host backup, physical-host disaster recovery, disk
  replacement, GPU passthrough, or bare-metal performance.

## Results

### Scope of the evidence

These are live measurements on `resolvr-box` on November 2, 2026, not a
documentation-only feature comparison. The completed substrate runs used Proxmox VE 9.2.0,
`pve-manager` 9.2.25, kernel `7.0.14-26-pve`, and QEMU package `11.0.3-4`.
Its management services were active and `dpkg --audit` was empty. The version
report separately flagged the legacy `pve-edk2-firmware` package; the inner guests
used BIOS boot, so this is not evidence of working UEFI templates.

The physical Ubuntu host was not converted to Proxmox. Its original guests kept
their assignments and running/stopped states. Each lab was deleted and the
original controller restarted before the next run. Final cleanup verifies no
lab VM, storage, network, or dataset remains, the original CPU policy matches
exactly, the physical pool is ONLINE, and original workload states match their
baseline. Curated measurements and cleanup results are in
[`lab/evidence/proxmox-spike.json`](../lab/evidence/proxmox-spike.json); the raw
private manifests are not suitable for committing.

### Hard tenant disk boundaries: feasible, with accounting caveats

The experiment gave Alice and Bob separate ZFS parents, each with a 16 GiB
`quota` and `reservation`, and registered each parent as its own Proxmox
`zfspool` storage with `sparse=0`. The base image occupied a separate parent.
Homecloud's physical-host controller was stopped throughout the storage tests.

- A bounded write under Alice's temporarily reduced quota was rejected with
  errno 122, `Disk quota exceeded`, after 96 MiB of an attempted 192 MiB.
  Bob still wrote and synced 32 MiB successfully.
- Snapshots were charged to the bounded hierarchy. Three snapshots retained
  50,528,256 bytes; a further bounded write failed at the 64 MiB child cap.
- These are practical host-enforced limits, **not byte-exact ceilings**. The
  first run's accounted usage exceeded the temporary quota by 24 KiB in the
  write case and 40 KiB in the snapshot case. These are observations, not
  universal upper bounds on accounting overshoot.
- Full clones of the checked Debian 13 cloud image booted as two real KVM guests,
  each with its root and cloud-init disks in its own tenant storage.
- An Alice disk resize of `+32G` failed because the tenant backing lacked space.
  Both guests could still write, sync, and read their proof data afterward.
- Each nominal 8 GiB root zvol had 8,726,249,472 bytes of `refreservation`:
  **130 MiB above nominal capacity**, before cloud-init disks, snapshots, and
  other accounting. A grant cannot simply equal the sum of displayed disk sizes.

The quota is the enforcement boundary; the parent reservation protects its
budget from siblings; non-sparse zvol backing reserves capacity for disks.
Those are distinct responsibilities. Keep physical-pool headroom as well as
tenant headroom. A capped tenant can still run out of its own snapshot budget,
and capacity reservations do not eliminate shared I/O contention or hardware
failure. This was not a saturation/performance test.

The tested mechanism used an owner/root helper to create the tenant datasets
and set ZFS properties, then the Proxmox API for registering storage and ordinary
VM operations. **An API-only tenant-quota implementation was not demonstrated.**
Central backups, image caches, snippets, and host logs were outside tenant
parents and need independent budgets and retention policies. They are not
silently covered by a tenant's VM-disk quota.

### Delegation and telemetry: usable substrate, not the product

An Alice-scoped Proxmox token could read her own VM's status. Reading Bob's VM
configuration returned HTTP 403 (`VM.Audit`), as did creating host storage
(`Datastore.Allocate`). This demonstrates selected resource-scoped denials,
not a complete least-privilege Homecloud provisioning role or hostile-tenant
security audit. Root credentials performed provisioning in the lab. The two
inner guests shared a test network; production network isolation was not built.

The API reported CPU, RAM, disk I/O, and virtual disk capacity. Its VM `disk`
field was **zero for both guests despite real data on disk**. A useful Homecloud
usage screen must distinguish:

- the person's granted budget;
- virtual disks allocated to their environments;
- ZFS-accounted use, including reservations and snapshot retention;
- actual guest filesystem use, collected separately if shown;
- shared/global storage and remaining physical-pool headroom.

In particular, ZFS parent `used` includes descendant reservations and must not be
labelled simply as physical bytes written by the coworker. Guest-reported
filesystem usage is useful display data, not the authority enforcing a grant.

### T3 lifecycle and recovery

The first T3 attempt downloaded the pinned T3 0.1.2 binary and installed Codex
0.165.0, but the native Claude 2.1.308 installer exited 134 before T3's boot
service was installed. The API-default guest CPU exposed no SSE4/AVX/AVX2 even
though the physical CPU supports newer instructions. The successful follow-up
recipe selects `cpu=host` only for the disposable Alice VM and explicitly
includes `libatomic1`. This supports using an explicit modern CPU recipe, but is
not an isolated experiment proving the first abort's cause.

The final recovery run passed:

- Real T3 **0.1.2** ran as an enabled user boot service, with loopback HTTP 200.
  Codex **0.165.0** and Claude Code **2.1.308** both ran their version commands.
- An independent heartbeat advanced from 4 to 14 lines across closed management
  calls while T3 stayed active. This tests independence from the management
  transport, not an authenticated T3 client session.
- A guest reboot changed its boot ID; T3 returned active/HTTP 200 and the
  heartbeat advanced from 28 to 33 lines after reboot.
- `vzdump` completed; the test VM was deleted and restored from its archive into
  Alice's storage. Both root and cloud-init disks were checked in `hc-alice`.
  The proof file survived, T3 returned active/HTTP 200 on a new boot, and the
  restored independent workload advanced from 52 to 58 lines.
- The restored CPU model remained `host`.

**Recovery revealed a real integration responsibility:** deleting the VM removed
Alice's VM-scoped authorization. Her token returned HTTP 403 after restore,
until the owner-to-VM ACL was reapplied. It could then read her running VM again,
while Bob's configuration remained HTTP 403. A product restore workflow must
reconcile ownership/access as well as disk contents and service readiness.

No user T3 Connect authorization, provider account authorization, authenticated
T3 client session, or actual model task was performed. Boot-service recovery
does not establish that an interrupted AI task resumes. Proxmox-node reboot and
guest autostart were not tested; the reboot case above is the inner guest only.

An intermediate follow-up passed the real T3 readiness, management-disconnection,
heartbeat, and changed-boot-ID checks and completed a `vzdump` backup. It then
stopped at HTTP 501 before VM deletion: the test client incorrectly put DELETE
parameters in a JSON body. The wrapper now sends them in the query string, with
offline GET/DELETE and POST regression tests. That intermediate fixture was
cleaned up, its CPU policy restored, its physical pool checked ONLINE, and its
original workload states verified. The final recovery run passed with the
corrected client; the intermediate failure is not evidence of a Proxmox restore
failure.

## Recommendation

**Continue with a small Proxmox-backed vertical slice; do not replace the running
Homecloud backend or reinstall the server based on this spike.** The storage,
template, API, and delegation results support substrate feasibility. They do
not establish customer demand, operational superiority over an incumbent, or a
finished coworker experience.

Homecloud should become a **team-environment product layer over Proxmox**, not
just a reskinned hypervisor UI, and not an attempt to remove all of its own
control-plane logic.

### What moves down, and what stays

Let Proxmox own VM execution, power operations, task execution, inventory,
ordinary template cloning, and VM backup/restore. Keep its native owner/admin
interface available for operations and recovery; do not rebuild the entire
ZFS/hypervisor administration surface first.

Homecloud still owns coworker identity and invitations, grants, admission,
environment ownership, the versioned T3 recipe, user-owned authorization,
access, comprehensible usage, and restore/offboarding workflows. It must record
and reconcile its desired state against Proxmox tasks: changing a backend does
not remove ambiguous timeouts, retries, drift, or orphaned-resource risks.

The existing `homecloud_host::Backend` request boundary in
`crates/host/src/lib.rs` is a useful place to prototype an adapter. However,
current requests include Incus/host-specific reserve and release semantics.
An adapter must explicitly support, revise, or reject those contracts, not
pretend an unsupported guarantee succeeded. No production adapter was added.

### CPU policy can be simpler

Start with shared vCPUs, an explicit CPU model compatible with the provider
binaries, conservative RAM admission, and optional CPU limits/weights. Do not
port the current exclusive physical-core distribution merely for parity.
`host` is a reasonable single-box recipe to evaluate, but a multi-host product
needs a compatible CPU baseline and separate migration testing.

The nested experiment shared only housekeeping CPUs for its outer worker. It
did not measure fair scheduling, sustained performance, exclusive allocation,
GPU assignment, or live migration. The actual hard storage boundary is separate
from whether CPU scheduling is shared or exclusive.

### Smallest useful product experiment

1. Connect Homecloud to one explicitly approved Proxmox node and bounded pool;
   validate the intended owner identity, storage, bridge, and capabilities.
2. Grant one coworker a disk budget, create a tagged tenant parent/storage using
   a narrowly scoped privileged helper, and admit one environment with backing
   overhead and snapshot headroom included.
3. Clone a versioned T3-ready template, show setup progress, and give the user a
   clear path to their own T3/provider sign-ins. Do not bake personal credentials
   into a template or reuse the administrator's provider login.
4. Show grant, allocation, accounted storage, CPU/RAM, and service health
   separately. Make safe resource changes work through the same permission model.
5. Test a real authorized coding task across laptop disconnection; separately
   test VM reboot, node reboot/autostart, offboarding, bounded snapshots, and
   restore. A boot service is not resumable model-task state.
6. Only then port more admin UX, recovery automation, and existing-VM workflows.

This is where a Nix recipe could fit: a reproducibly built environment/template
is an input to this product layer, not a reason to implement a new hypervisor.
No Nix-based Proxmox image build or import was tested here.

The validation target is an owner and coworker who can provision, use, and
recover a persistent environment without coordinating multiple control planes.
If they are satisfied with stock Proxmox plus a prepared template, this spike
has shown an implementation option, not a differentiated product.

## Primary references

- [Proxmox ZFS storage backend](https://github.com/proxmox/pve-docs/blob/master/pve-storage-zfspool.adoc)
- [OpenZFS dataset properties](https://openzfs.github.io/openzfs-docs/man/master/7/zfsprops.7.html)

The live measurements above remain distinct from documentation-derived semantics.
