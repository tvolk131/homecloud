# Shared Nix store hardening with Fedimint

This follow-up uses the existing Rust lab runner and stock Nix 2.35.4. It is a
shipping investigation, not a production service or an enabled Homecloud feature.
The first proof and its limits are in `nix-shared-store-prototype.md`.

## Result on October 31, 2026

The two consumers successfully used Fedimint's default Nix development shell
and compiled/tested three real project crates against its Rust toolchain.
Each VM passed 17 unit tests (5 AEAD, 9 HKDF, 3 error-formatting); the three
doc-test suites contained no tests. The source revision was
`17336264b340f01b1cd786a2a46bb14b5da132af`. No Fedimint source changes were made.

This supports continued development of the architecture, but not shipping this
prototype as a transparent managed development VM. Runtime GC safety and the
filesystem settings needed for real compiler workloads are release blockers.
The reviewed numerical evidence is in
[`lab/evidence/nix-fedimint-hardening-20261031.json`](../lab/evidence/nix-fedimint-hardening-20261031.json).

| Check | Observed result |
| --- | --- |
| Default Fedimint shell | Both clients resolved `/nix/store/fkncgqbqxi5c2pf58bv8m1lg5fivmb8n-nix-shell` |
| Independent compilation | Both clients passed the same 17 selected unit tests using private Cargo targets |
| Shared package files | Host and both guests reported inode 321547 for the same resolved Rust toolchain file |
| Second-client package allocation | Host `/nix/store` allocation stayed at 3,211,183,616 bytes before/after final reuse |
| Concurrent requests | Both guests received the same verified worker-produced proof output |
| Live additions with data caching | Previously absent output became executable without a remount; mount ID 115 stayed unchanged |
| Full host GC with development profiles | Rust, Cargo and OpenSSL still executed in both held-open Fedimint shells after collection |
| Endpoint outage and reboot | Existing program ran with endpoint stopped; requests failed as expected; reboot restored untrusted store access and read-only mount |
| Cross-client root creation | A could not create an output root inside B's state directory |
| Unrooted running program | Host GC and an untrusted B deletion request each removed A's output; its later file read failed |
| Explicit permanent root | Prevented deletion; late read succeeded; deletion succeeded after root removal |
| Bounded Rust GC guard | Host- and guest-requested collectors waited; fresh worker publication and late read succeeded; collection finished after release |

## Disk accounting

The final warmed shell closure contained 414 paths and 7,266,481,936 bytes of
serialized NAR data. At measurement time, the whole publishing store occupied
3,211,183,616 allocated bytes; its ZFS dataset used 3,286,007,808 bytes and
reported 8,225,004,544 logical bytes. These measurements cover different things:
NAR bytes are neither filesystem allocation nor a measured savings baseline.
The store was already populated by earlier diagnostic attempts, so this is not
a cold-start benchmark.

The final A run grew store allocation by 8,192 bytes; B's run grew it by zero.
Both read the same host-backed package files rather than installing a package
store in each guest disk. This does not eliminate other storage: the worker
keeps its own Nix build store; source/workspace files and small profile/config
state are per client; Cargo target directories used 109,518,848 bytes in A and
109,522,944 bytes in B on their private root disks. Downloads, evaluation caches,
VM operating systems and transient build space also remain separate. RAM/page
cache deduplication was not measured.

## Compatibility findings and fixes

The original proof's newer guest kernel and POSIX ACL requirement still apply.
The consumers ran `7.0.0-42-generic`, stock Nix 2.35.4, and untrusted SSH store
identities. The host kernel was `7.0.0-38-generic`; Incus was 6.0.5 and the installed
virtiofsd package was `1.13.2-6ubuntu0.1`.

Fedimint exposed two additional filesystem problems:

- A shared SQLite fetch cache returned disk I/O errors. Host and guest now use
  separate private cache directories at the same per-client absolute path.
  Canonical names matter because some evaluation runs on the remote store side.
- Wild, the linker selected by the project, could not mmap glibc's `Scrt1.o`
  with `io.cache=none` or `metadata` (`No such device`). Both consumers passed
  with Incus `io.cache=unsafe` on the server-enforced read-only store export.
  `auto` was rejected by this Incus, and `writeback` failed device startup.
  The installed virtiofsd offers `--allow-mmap`, but this Incus configuration
  does not directly expose it. We did not replace global daemon binaries.

`unsafe` is the actual Incus setting, not a claim of production safety. A
read-only client export does not make the host store static: publication and
GC still change it. One live-addition/reboot sequence passed with this mode;
long-running coherence, removal/republication, failure and upgrade behavior
remain to be established. `install clients` retains the original uncached
baseline; `install data-cache` explicitly opts lab exports into this workaround.
Some private transcript filenames say `writeback`; those final checks actually
ran with `unsafe`, as the recorded device configuration confirms.

Two harness defects were fixed during the investigation. Streaming shell code
on SSH stdin let child commands consume unread script text; the runner now
receives the whole script before executing it with closed stdin. Shell logging
through `if (body)` suppressed failure propagation; logging now executes the
body in a separate `sh -e`. Focused regressions cover both. Earlier Fedimint
attempts with Cargo failures were incorrectly reported as successful by the
old harness and are excluded from pass claims. The final A/B passes used the
corrected runner. Reboot readiness now waits for an actual store connection,
in addition to a changed boot ID and a restored mount, because the guest agent
can return before DHCP finishes.

## Runtime retention is a release blocker

The host collector cannot discover guest process roots by scanning the host's
processes. An ordinary unrooted `nix run` remained running while its output was
collected, and failed when it later opened a file from that output. An
**untrusted second client** could trigger this deletion through the SSH daemon.
Disabling a host automatic-GC timer is therefore insufficient.

A permanent shared result/profile root protected the test. A second experiment
used the Rust `nix-lab-gc-lease` helper to hold a shared filesystem lock on
`/nix/var/nix/gc.lock`; the collector waited for the exclusive lock. Tests with
both a host collector and an untrusted client collector preserved the running
program's late read and allowed new worker outputs to publish while held.
Collection completed only after the program and lease exited.

This helper has a 300-second lab deadline and is **not a VM-lifecycle service**.
A production coarse policy could defer all collection while a consumer is
active, but must hold protection before admitting the VM, cover publication,
startup, crash/reconnect, controller/service restarts and host reboot, and
fail closed until uncertain consumers are reconciled. It also needs a disk
pressure policy: deferring all GC for long-lived VMs can prevent reclamation
indefinitely. Fine-grained guest runtime leases would need a separately
validated protocol. Neither policy is implemented here.

## Workload and boundaries

Fedimint is taken from a clean local HEAD using `git archive`. Untracked files,
Git remotes, credentials, hooks, and the user's local Cargo configuration are not
copied. Each fixture gets a fresh Git repository over that snapshot. Record the
original revision separately: the fixture commit is synthetic and its Git hash
is not the upstream revision.

The default Fedimint development shell is built with `nix build
.#devShells.x86_64-linux.default`; the ordinary project-local `result` link is
intentional. `nix develop --command ...` uses the default development-profile
behavior. Inside that environment, Cargo builds and tests `fedimint-hkdf`,
`fedimint-aead`, and `fedimint-util-error` in each consuming VM's private target
directory. This distinguishes shared Nix toolchain packages from ordinary
per-VM Rust build outputs. It is not a full Fedimint workspace build or federation
integration test.

Each client's workspace and XDG state live in its own writable export at the
same absolute path on host and guest. Fetch/evaluation caches stay on the
client's private root disk under a per-VM canonical cache path. The host has a
separate private cache at the same path; these cache directories are not
exported or shared. `/home/dev/fedimint` points
to that workspace. This arrangement exercises ordinary commands at a managed
workspace location; it does not establish transparent operation in arbitrary
guest-private directories. The shared package store remains read-only in both
clients. Fedimint's explicitly published cache/signing key is configured on the
host and builder; guest identities remain untrusted.

## Regression cases

- Run the original proof from fresh fixtures, including negative lookups, profile
  roots, endpoint outage, reboot, write denial, and untrusted-option overrides.
- Request a fresh derivation concurrently from both clients and check that both
  receive the same verified worker-produced output.
- Run `nix run` without a permanent root; attempt targeted collection while the
  program waits, then have it open a file in its own output. Record failure as a
  retention finding, not a passing correctness result.
- Repeat with an explicit root held for the program's lifetime. Require the late
  read to succeed, then release the root and verify that collection succeeds.
- Hold real Fedimint development shells open in both guests during full host GC;
  start the Rust toolchain and OpenSSL after the collector finishes.
- Compare toolchain path/inode, store allocation, guest mounts, and private Cargo
  target allocation. Do not equate NAR sizes, allocated bytes, or ZFS compression.

## Reproduction

The runner captures actual protected-VM process IDs using Incus state queries.
Fixture sizes and the shared-store quota/reservation are persisted in its private
manifest; older manifests default to the original 16 GiB shared-store size.
The larger workload uses two 2 GiB/12 GiB clients, an 8 GiB/64 GiB worker,
and a separate 64 GiB shared dataset. All fixtures have one physical-core
allocation. Reserve host capacity and coordinate host access before provisioning.

```sh
cargo build -p homecloud-nix-lab
# Keep --state, --ssh-config and --cli fixed throughout the run.
# Additional sizing arguments are used when first preparing the manifest:
homecloud-nix-lab ... --builder-memory-gib 8 --builder-disk-gib 64 \
  --store-gib 64 prepare
```

Run the original setup/exercises described in the first report, then
`exercise concurrency`, `exercise root-isolation`, `exercise runtime-gc`,
`exercise runtime-client-gc`, and `exercise runtime-lease`. The two unguarded
runtime-GC phases are diagnostic reproductions of the retention bug, not
correctness passes. To reproduce the lock checks, first build the native Rust
`nix-lab-gc-lease` helper on the Linux host and put it at the run base as
`gc-lease`; then use `exercise runtime-gc-lock` and
`exercise runtime-client-gc-lock`. The helper is deliberately bounded and
requires explicit staging.
Use `install data-cache` for the tested mmap workaround, then `fedimint source --source /path/to/clean/fedimint`, `fedimint metadata`,
`fedimint develop`, `fedimint reuse`, `fedimint measure`, and
`exercise full-gc`. Source staging is deliberately not blindly repeatable.
Private, phase-specific transcripts stay in the run's host audit directory.
`fedimint cache` reapplies only the private-cache configuration; it was used to
resume the first hardening campaign after diagnosing the shared-cache failure.
Source staging now applies that same private-cache setup automatically.

Uninstall the lab services and exports before deleting the manifest-owned VMs.
Restore the grant, any temporary disk-budget migration, and the original running
VM's state. Preserve the metrics configuration, credentials, controller and
other VMs. Never restore an old controller database over current state.

## Remaining shipping gates

In addition to the runtime-retention and filesystem blockers above, this run
does not establish safety across host reboots,
forced power loss, filesystem-service interruption, version upgrades, storage
exhaustion, malicious-client stress, or a long-running development soak. Runtime
retention needs an explicit Homecloud policy in addition to profile roots.
Shared store contents are visible to all attached VMs; they must belong to the
same visibility/trust group.

## Cleanup and validation

All three fixture VMs and the tagged shared dataset were removed. The host's
Nix services, exports, identities, firewall exceptions, and marked temporary
Rust toolchain were removed. The original owner grant and 128 GiB managed disk
budget were restored, without replacing the controller database. Both host
configuration files compare canonically equal to the before-budget copies;
ownership and modes match. The metrics listener and certificate hashes match
the baseline. The development VM is running with its original specification and
resources and a responsive guest agent; the other running VM retained its
original process ID, and the originally stopped VM remains stopped. The host
was explicitly released to the telemetry thread. Only inactive root-owned audit
artifacts/backups and an archived root fetch cache remain on the host.

The runner's four focused tests, all-target Clippy with warnings denied, formatting
and whitespace checks passed. Fedimint's original source checkout remains clean.
This change is confined to the Rust lab runner and evidence/docs; it is not wired
into the production Homecloud controller or GUI.
