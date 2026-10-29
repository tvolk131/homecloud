# Managed Homecloud host lab

`homecloud_host` is a managed application, alongside `t3`. Its VM uses the normal
identity, resource ledger, operation lane, stop/release rules and owner checks.
For now, creating or reinstalling it requires an administrator. It is a disposable
host environment, not a new machine registration or a production backup.

The VM runs its own Linux, Incus, ZFS and Rust controller. It has independent
accounts, endpoint keys, SQLite state and guest VMs. No outer host credentials,
provider sessions or database are copied into it. Its system services start on
boot even if its administrator has not been paired yet. Inspect is read-only;
installation/repair is explicit, so inspecting a fault never repairs it silently.

## Provisioning

Prepare a root-owned `managed_host_image` fingerprint in the outer host profile.
The lab pins Ubuntu 26.04/cloud to match its OpenZFS 2.4.1 and Incus 6.0 environment;
ordinary VMs retain their existing image. The cloud image keeps its native agent-based cloud-init transport. The guest profile excludes the worker’s uplink subnet from inner VM bridge allocation, avoiding overlap without a new network fabric. Nested KVM must be available in the
worker. The fixed guest installer checks `/dev/kvm` before installing the host.

Build Linux `homecloudd` and `homecloud`, then package only reviewed software:

```sh
python3 scripts/bundle_host.py --bin-dir target/debug \
  --output /tmp/homecloud-host-bundle.tar.gz
sudo install -m 0644 /tmp/homecloud-host-bundle.tar.gz \
  /usr/local/libexec/homecloud/homecloud-host-bundle.tar.gz
```

The bundle contains binaries, fixed helpers and installers. It excludes state,
profiles and secrets. The outer adapter supplies and verifies its SHA-256 before
extraction. Archive paths and member types are restricted.

Choose **Homecloud host lab** in Create VM, or:

```sh
homecloud create lab-host --homecloud-host --memory 8 --cores 2 --disk 32 \
  --ssh-key ~/.ssh/id_ed25519.pub --request-id REQUEST_UUID
```

Minimums are 8 GiB RAM, two dedicated cores and a 32 GiB root disk. A separate
32 GiB block volume is also charged against disk backing: the example holds
66 GiB with a 2 GiB overhead budget. Stop releases compute; both disks remain
backed. Delete removes the tagged test volume only after its VM has been deleted
and Incus reports no attachment. Foreign or still-attached volumes are retained.

The test pool uses a deterministic WWN, verified size and a durable pool-creation
intent. The installer refuses unrecognized signatures, never guesses an empty
physical drive, and imports its existing pool on retry. This nested ZFS pool is
for experiments; its vdev remains backed by the outer ZFS pool.

Inspect progress in VM details. An explicit retry is also available:

```sh
homecloud app-install VM_UUID --homecloud-host
```

Installation includes Incus, ZFS, QEMU, firewall/CPU prerequisites and
`genisoimage` for cloud-init seed disks. Parent directory permissions are explicit
under a restrictive job umask. Installation waits for the controller endpoint
before reporting success. Management does not wait behind Incus startup, and read-only Incus calls have an eight-second bound. Initial administrator enrollment uses local authority
inside the worker; test enrollment consumes its local ticket without emitting
tokens or credentials.

## External supervisor and memory campaign

The supervisor runs outside the worker. It retains a private manifest with exact
VM IDs, owner, name, original grant and idempotency keys. Remote guest commands
verify those values and the Incus ownership tags. The managed-host probe is
separate from the existing ordinary-VM probe, whose restrictions remain intact.

```sh
python3 lab/managed_host_runner.py boot --state /tmp/hc-host-lab/state.json \
  --ssh-config /path/to/pinned-ssh-config --cli target/debug/homecloud
python3 lab/managed_host_runner.py status --state /tmp/hc-host-lab/state.json \
  --ssh-config /path/to/pinned-ssh-config --cli target/debug/homecloud
python3 lab/managed_memory_campaign.py --state /tmp/hc-host-lab/state.json \
  --ssh-config /path/to/pinned-ssh-config --cli target/debug/homecloud
python3 lab/managed_host_runner.py cleanup --state /tmp/hc-host-lab/state.json \
  --ssh-config /path/to/pinned-ssh-config --cli target/debug/homecloud
```

The campaign uses the outer API to stop, resize and start the worker. It proves:

- The inner controller and a real inner VM boot on the initial worker.
- Growing 8 → 36 GiB leaves the old reviewed capacity/grants unchanged until
  adoption. Existing held VM intent resumes once on the new kernel boot.
- A cap below the grown host's effective ARC minimum coordinates min then max,
  verifies effective ARC stats, and persists numeric bounds for reboot.
- Admin adoption expands the VM envelope without rewriting users or claims.
- Shrinking to 8 GiB with 6 GiB held keeps the claim and blocks guest autostart.
  Management stays reachable. Unsafe running edits are rejected before intent.
- Explicit Stop verifies deprovisioning before release. A stopped resize plus
  grant correction permits restart, retaining identity and the proof file.

Completed stages are checkpointed; a failed operation remains available for
inspection. A reviewed retry is explicit. Cleanup restores the original outer
grant and removes only the manifest-owned worker and its associated test disk.
Nested tests do not prove GPU/IOMMU behavior, hardware power-loss durability, or
full-density physical-host memory performance.

## Deterministic tests

Keep real SQLite transactions and independent database connections. The Rust
backend scenario separates actual effects, observations and replies: QEMU may
exist before Running is observed; a response may be lost after an effect; stopped
power does not prove CPU/GPU cleanup. Tests advance these events explicitly,
without sleeps, and keep claims until the final attestation.

`lab/models/arc.py` is a small model of the Linux OpenZFS 2.4.1 tuning boundary,
not a filesystem simulator. Production `arc_policy.py` runs against modeled
knobs, effective stats and temporary config files. It covers ignored writes,
min/max ordering, boot persistence and explicit cache reclamation. Recorded real
probes calibrate these assumptions. New behavior needs a real probe before
expanding the model; do not teach a fake what we merely hope upstream does.


The clean-install and degraded-management follow-up is automated by
`lab/managed_host_smoke.py` and `lab/managed_host_recovery.py`, using the same
manifest arguments. A service-fault fixture must restore `incus.socket`,
`incus.service` **and** `incus-startup.service`; the latter owns Incus's normal
shutdown hook and is stopped when its required socket is stopped. The cold-boot
proof checks both an fsynced and an ordinary buffered file. Abrupt power-loss
tests should require durability only for explicitly synced writes.

One nested shrink boot produced an unexplained Incus SIGKILL without a kernel
OOM event; an explicit restart recovered it. This did not recur on the fresh
worker's boots. The cause remains a recorded investigation item, with successful
memory-claim fencing and independent management availability established.

## Two-user memory recovery campaign

`managed_multi_memory.py` extends the proof to two ordinary users with independent
device profiles and one real VM each. Enrollment secrets and profiles stay inside
the disposable worker. Provision it with three cores so its own controller can
reserve one and assign the other two exclusively:

```sh
python3 lab/managed_host_runner.py boot --worker-memory 12 --worker-cores 3 \
  --state /tmp/hc-multi/state.json --ssh-config /path/to/pinned-ssh-config
python3 lab/managed_multi_memory.py --state /tmp/hc-multi/state.json \
  --ssh-config /path/to/pinned-ssh-config --cli target/debug/homecloud
python3 lab/managed_host_runner.py cleanup --state /tmp/hc-multi/state.json \
  --ssh-config /path/to/pinned-ssh-config
```

Wait for managed installation before starting the campaign. It checks a 12 → 10 →
8 GiB worker sequence. Six GiB of promises with four GiB held allows both VMs to
resume against the intermediate five-GiB budget, while the three-GiB budget blocks
both and preserves their claims. Stopping one owner's VM releases its compute
only after cleanup and lets the other's existing claim resume. A controlled ARC
mismatch changes the shared health condition and explicit policy reapplication
restores it without starting guests into the remaining shortage.

The last stage stops/resizes both VMs to one GiB, grants each one GiB, and races
four Start requests with two competing two-GiB grant targets. Only one grant can
spend the final GiB; each VM gets one distinct core and one live QEMU process.
Both fsynced proof files must survive. Owner snapshots must contain only their
own VM/user and omit aggregate health details. Assertions use real controller
responses and process observations; no private credentials enter evidence.

The real cache-repair probe found a previous conflation: the adapter's `applying`
flag also meant ARC drift. That blocked explicit repair even when no reserve job
was in progress. The flag now reports the journal phase; ARC readiness remains a
separate attestation. Admission, automatic workload reconciliation and reserve
completion still require ARC enforcement, while an explicit new repair can be
submitted from a completed journal. An actually unfinished reserve job continues
to block a second operation. Negative tests cover both cases and refuse an applied
receipt with unenforced ARC.

An initial host-configured cache setting has no managed ARC mode. Repair must
select Automatic or a concrete custom bound; an unspecified mode is refused
before effects if ARC is unsafe. The GUI proposes a backed bound for explicit
approval. The campaign selects a 2 GiB custom limit. The first mode-less attempt
was retained as a finding: its completion was refused and claims stayed fenced.
Only the synthetic fault was rolled back to finish that receipt before the
corrected repair proof resumed on the same VMs.
