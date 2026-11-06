# Disposable real-PVE cockpit validation

Date: November 2, 2026. This is a separate experiment from the earlier HTTP-mock
adapter tests. The physical `resolvr-box` is Ubuntu/Incus, not Proxmox. It must
not be reinstalled to run this validation.

## Safety and reproduction

The runner reuses the manifest-owned nested-VM preparation, Debian/PVE package
installation, reboot, and ownership-checked destruction from
`lab/proxmox_spike.py` and `lab/proxmox_guest.py`. It does **not** invoke the
storage quota-fill, template-cloning campaign, or T3 workload from that spike.
The existing harness reserves an isolated 128 GiB ZFS parent and provides a
32 GiB VM root plus an unused 64 GiB scratch volume. Only these tagged resources
are disposable; existing VMs, storage, and the physical OS are not.

The new entry point is:

```sh
python3 -m unittest discover -s lab -p 'test_proxmox*.py' -v
python3 lab/proxmox_cockpit_live.py run \
  --state lab/evidence/private/proxmox-cockpit-live-20261102.json
```

The default pinned SSH configuration is
`~/.config/homecloud/host-access/resolvr-box/ssh_config`, alias `hclab-host`.
The private manifest is mode 0600. It records source hashes and host baselines,
not API token values. Do not publish the complete baseline: original guest
configuration can contain public SSH keys and deployment metadata.

The runner records original controller activity, Incus VM states/configuration,
storage/network/dataset identities, CPU-policy content hash/mode, and physical
pool health before mutation. It also checks physical package-database and CA
bundle hashes and OpenSSL shared-library content/ownership on the final run.
The existing harness temporarily stops
`homecloudd` and appends an owned CPU-policy entry for the worker; existing guest
power states are left alone. Cleanup restores the CPU policy and original
controller activity, checks the recorded baseline, and removes the tagged VM,
network, storage, dataset, transient units, remote manifest, and private build.
`run` performs cleanup in `finally`, including installation/build/probe failure.
If the process or SSH connection is externally interrupted, use the retained
manifest rather than inventing resource identities:

```sh
python3 lab/proxmox_cockpit_live.py cleanup \
  --state lab/evidence/private/proxmox-cockpit-live-20261102.json
```

An existing manifest is cleanup-only. Use a new filename for a new run. Cleanup
may force-stop only the ownership-checked disposable worker if graceful stop
fails. An externally modified CPU policy or differing original VM configuration
is a failure requiring inspection, not permission to overwrite unrelated work.
Incus auto-updated image-cache datasets are recorded separately if they change;
they are not deleted or rolled back to manufacture whole-host equality. All
non-cache dataset changes still fail cleanup verification.

## What the new probe exercises

- `lab/proxmox_cockpit_host.py` snapshots current source into an isolated remote
  workspace and private Cargo cache, including the uncommitted adapter. The
  official `proxmox-client` git revision remains pinned to
  `89c1d597a9d15743049eadcb1add380573265904`. The remote Rust toolchain is 1.99.
- The host's OpenSSL development headers are obtained with `apt-get download
  libssl-dev` and `dpkg-deb -x` inside the owned build directory, with private
  links to existing OpenSSL 3 shared libraries and `OPENSSL_STATIC=0`. No physical
  host packages are installed. Compilation is a bounded transient unit on
  housekeeping CPUs, with three Cargo jobs and a 6 GiB memory limit.
- `lab/proxmox_cockpit_guest.py` seeds only the disposable PVE instance: two
  stopped, 512 MiB/two-vCPU QEMU guests with sparse 1 GiB disks, one marked as a
  template, and a dedicated directory storage. No inner guest workload is booted.
- A dedicated PVE user and privilege-separated token both receive `PVEAuditor`
  on `/`. The token expires after four hours; its value stays in private files.
  A second token has no token ACL, despite the user's auditor role. A forbidden
  write targets only the disposable stopped VM and must return HTTP 403 with
  its original name unchanged.
- The PVE certificate SHA-256 digest comes independently from the owned VM over
  pinned SSH/Incus exec. A separate HTTPS reference read validates the PVE CA
  and that digest. The production adapter uses its normal fingerprint option,
  not an insecure transport or an HTTP mock.
- `lab/proxmox_cockpit_probe.rs` calls production `connect_proxmox` and
  `VirtualizationBackend::inventory`, then uses production `Service`, `Store`,
  `serve`, and `Client` to enroll an owner and fetch the same real-PVE inventory
  through authenticated loopback Iroh QUIC. Member/unknown-device access,
  revocation, and legacy snapshot rejection are checked. The fixture member is
  seeded directly into the fresh probe store; this does not enable member
  invitation through the owner-only product RPC. Production probe processes run
  as the unprivileged host user `tommy`, with private token/config files and all
  Iroh/SQLite temporary state inside the owned build directory.
- Negative adapter cases cover invalid token secrets, an incorrect certificate
  fingerprint, default CA verification of the untrusted private PVE CA, and
  group/other-readable token files. The unprivileged token must not expose VM
  or storage inventory. Probe output is checked for token-value leaks.
- A separate positive case exercises production default CA verification using
  the private PVE CA via `SSL_CERT_FILE` for that probe subprocess only. This
  does not install a CA or change the physical host's trust configuration.
  Both fingerprint and process-local CA trust are exercised through Iroh too.
  A CA-valid certificate with a deliberately wrong configured fingerprint is a
  separate positive case: the pinned upstream client's fingerprint mode accepts
  normal CA validation **or** fingerprint fallback, not exclusive leaf pinning.

## Results and cleanup

**Passed.** Final run `a714fb71790e41239f4ea9c6c11f3af3` used
`pve-manager/9.2.25/88a31f5fd94aab6d`, running kernel `7.0.14-26-pve` inside the
disposable VM. `pveproxy`, `pvedaemon`, and `pvestatd` were active and
`dpkg --audit` was empty. Production probe processes ran as host UID 1000.
The final run lasted 869 seconds, from November 2, 2026, 22:45:23 to 22:59:52 UTC.

Sanitized evidence: `lab/evidence/proxmox-cockpit-live.json`. It includes exact
source hashes, the compiled probe hash, effective audit-only permissions, native
PVE reference rows, mapped inventories, positive/negative cases, baseline check
booleans, and the two earlier failed-attempt summaries. No token values, device
secrets, or original guests' full configuration are published. All published
adapter/server source hashes and the probe hash match the checkout after
execution. A separate pinned-SSH read-back after all three attempts confirmed
all their owned resources and all six transient units were absent; that
independent check is appended to the evidence artifact.

| Check | Observed result |
| --- | --- |
| Real HTTPS inventory | One online node, two stopped QEMU records, two available directory storages; source `proxmox` |
| Template mapping | VM 7101 false; VM 7199 true after native/read-only API readiness settled |
| Configured guest capacity | Each VM: two vCPUs, 536870912-byte memory limit, 1073741824-byte reported disk capacity |
| Node/storage mapping | Four node CPU threads; native node RAM capacity and both 33124446208-byte storage capacities mapped correctly |
| TLS | Verified fingerprint and process-local CA trust pass; untrusted default CA and wrong fingerprint without CA trust fail |
| Upstream TLS policy | A valid CA chain plus a deliberately wrong fingerprint succeeds: fingerprint is fallback trust, not exclusive pinning |
| Token authentication/privacy | Invalid token fails authentication; mode 0644 secret rejected; private mode 0600 succeeds; no token values in output |
| PVE permission separation | No-token-ACL token exposes node identity/status but null node metrics and no VM/storage rows |
| Read-only scope | Forbidden rename of owned VM 7101 returns HTTP 403; name unchanged; effective permissions are seven `*.Audit` privileges |
| Authenticated Homecloud Iroh | Owner gets real inventory with both TLS trust modes; member, unknown device, revoked device, and legacy snapshot denied |
| Product-state isolation | No legacy VM records created; no original guest power/configuration changes |
| Forward-compatible rows | Real PVE `network` resource row ignored rather than misrepresented as a supported inventory kind |

No production mapper/client defect requiring a parent Rust change was observed.
The apparent template discrepancy on the earlier attempt was an unsettled native
PVE response, not disagreement between that response and the Rust mapper.

Executed final commands:

```sh
python3 lab/proxmox_cockpit_live.py run \
  --state lab/evidence/private/proxmox-cockpit-live-20261102-settled.json
python3 lab/proxmox_cockpit_live.py report \
  --state lab/evidence/private/proxmox-cockpit-live-20261102-settled.json \
  --previous-state lab/evidence/private/proxmox-cockpit-live-20261102.json \
  --previous-state lab/evidence/private/proxmox-cockpit-live-20261102-retry.json \
  --output lab/evidence/proxmox-cockpit-live.json
```

The final `run` exits 0 and its assessment has no failures. The isolated Linux
commands below passed: eight virtualization tests, the production daemon and
live probe build, and dependency-tree confirmation of `proxmox-client` 1.0.4 at
the pinned git revision. The only reported dependency warning was the existing
future-incompatibility warning for `syn` 0.12.15.

```sh
cargo test --offline -p homecloud-virtualization --features proxmox --lib
cargo build --offline -p homecloud-cockpit-live-probe -p homecloud-daemon
cargo tree --offline -p homecloud-cockpit-live-probe -i proxmox-client
```

The daemon binary was compiled, not installed or substituted for the physical
host's existing controller. The executed Rust probe uses the production adapter,
service, and authenticated transport libraries directly. The local lab safety
suite passed all 26 tests with:

```sh
python3 -m unittest discover -s lab -p 'test_proxmox*.py' -v
```

### Final cleanup state

- Original native VM states/configuration matched exactly: two Running, one
  Stopped. No existing VM was paused, stopped, deleted, or reconfigured.
- `homecloudd` active as before; original CPU-policy bytes/hash/mode restored.
- Every recorded final baseline field matched: OS, controller, original VM
  configuration/power, Incus storage/network lists, full ZFS dataset list,
  physical package database, CA bundle, OpenSSL contents/ownership/mode, parent
  pool properties, root-rustup absence, and ONLINE physical-pool health.
- All three attempts' owned worker VMs, inner resources/tokens, outer storage
  pools/volumes, networks, parent datasets/reservations, host manifests, private
  builds/Cargo caches/OpenSSL headers, and transient units were removed. The
  final worker stopped gracefully; no force-stop was needed. Both final unit
  load states were `not-found`.
- Only local private manifests and the sanitized evidence remain, plus normal
  host audit/journal entries. The first attempt's unrelated Incus auto-updated
  Ubuntu image-cache change remains untouched and is recorded separately.

### Preparatory failure, not adapter evidence

The first run, `66a07a4f76944851818f52aedee49ce4`, installed and rebooted real
PVE, seeded the stopped VM/template, and confirmed the auditor token's forbidden
write returned HTTP 403. The production probe and daemon compiled, but an
unqualified `cargo tree -i proxmox-client` inspected workspace default members
without the probe's enabled feature and failed its reporting step. Inventory
validation therefore did **not** run in that attempt. The command now explicitly
selects `-p homecloud-cockpit-live-probe`; toolchain reporting also supplies the
correct user's `HOME` and `RUSTUP_HOME`.

The initial whole-dataset equality guard also detected a separate Incus image
cache update at November 2, 2026, 22:17:56 UTC. The auto-updated Ubuntu Noble
image changed from fingerprint `2139edd9…` to `9f0c1330…` across existing pools.
The fixture used the already-cached Debian Trixie image `011ecfa2…`, uploaded
before this run. Only image-cache dataset identities differed; original VM
states/configuration, controller state, CPU policy, and storage/network lists
matched. The revised verifier reports this cache churn explicitly and never
touches those non-owned datasets. A read-only toolchain diagnostic also created
an empty `/root/.rustup`; after verifying it was empty, `rmdir` restored its
original absence.

Explicit cleanup of that preparatory attempt completed: owned VM/storage/
network/dataset, remote manifest/build, and active transient units were absent;
the controller was active, CPU policy restored, original three VM configurations
and power states unchanged, and physical pool ONLINE. This attempt is not
counted as successful official-client live validation.

### Live retry caught an unsettled reference

Run `176c885caef54e399bb602f146e3bc4d` executed the real official adapter and
both authenticated Iroh paths successfully as UID 1000. HTTPS token/TLS/private
file failures and owner/member/unknown/revoked-device checks behaved as expected.
It was nevertheless marked **failed**, not silently accepted: VM 7199 had just
been converted with `qm template`, but the reference `/cluster/resources` read
still reported `template: 0`. The adapter also reported false, faithfully
mapping that upstream response. This is not evidence of a Rust mapper defect.
The readiness check had waited for VM capacity but not the template flag.

The final seed now verifies `template: 1` in `qm config`, and waits for both the
native root API and the actual read-only HTTPS token to observe the template
flag and configured capacities before invoking the production adapter. This
keeps eventual PVE statistics propagation separate from response mapping.

Cleanup for the live retry passed exact baseline comparison, including physical
package database/CA/OpenSSL hashes and original VM configurations/power states.
Every owned resource and remote build/manifest was absent, both transient units
were `not-found`, the controller was active, CPU policy restored, and `hclab`
ONLINE; force-stop was not needed.

## Scope limits

This validates only read-only owner inventory and the exercised failure paths.
It does not validate physical-host migration, a production PVE install on this
hardware, coworker provisioning, VM start/stop/clone/recovery, disk enforcement,
LXC inventory, multi-node clusters, public CA renewal, or GUI rendering. The
Iroh hop is authenticated real QUIC on loopback, not a laptop-to-host relay/NAT
or desktop end-to-end test. Dynamic CPU/memory/disk usage can change between the
reference read and adapter read; configured capacities and identities are the
stable mapping assertions. The two directory storages in this fixture share a
backing filesystem: their reported capacities must not be summed as independent
physical capacity. Running-guest utilization, CPU/memory hotplug, and hardened
service-unit packaging were not validated.

## Changed files

- `lab/proxmox_cockpit_live.py`: manifest supervisor, failure cleanup, assessment,
  and sanitized evidence export.
- `lab/proxmox_cockpit_host.py`: ownership-checked baseline/build/HTTPS/Iroh
  execution and cleanup verification over pinned SSH.
- `lab/proxmox_cockpit_guest.py`: stopped fixture guests, read-only tokens,
  template-cache readiness, and forbidden-write assertion.
- `lab/proxmox_cockpit_probe.rs`: disposable driver of production Rust libraries;
  no Rust crate source was edited.
- `lab/test_proxmox_cockpit_live.py`: offline ownership, cleanup, publication,
  native-template readiness, and response-assessment regression checks.
- `lab/evidence/proxmox-cockpit-live.json`: sanitized live evidence and failed
  attempt history.
- `docs/proxmox-cockpit-live-validation.md`: this report. Existing crate changes,
  `docs/proxmox-cockpit.md`, root README, and existing lab harness files were
  preserved; no commits or branches were created.
