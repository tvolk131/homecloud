# Repeatable integration proofs

Use the pinned SSH configuration for the dedicated lab host and the authenticated
Homecloud CLI. Proof VMs have a per-run `hc-proof-<uuid>` prefix, owner ID, stable
request keys and a versioned, fsynced manifest. Failed proofs retain resources for
inspection; a timeout is never permission to free a claim or force power off.
Run only one grant-changing proof at a time.

## Management SSH access

Keep the independent, pinned management connection outside every checkout at
`~/.config/homecloud/host-access/resolvr-box/ssh_config`, alongside `known_hosts`.
This is separate from Homecloud's paired Iroh client profile. The private key
stays in `~/.ssh`; use mode 0700 for the access directory and 0600 for its files.
Preserve `StrictHostKeyChecking yes` and the existing host-key pins when moving
the files. Update absolute `UserKnownHostsFile`, `Include`, and `ProxyCommand`
paths; do not discover or replace trusted keys with an unverified key scan.

Pass this path explicitly to runners with `--ssh-config`. Older helpers using
`remote.py`, `fetch_evidence.py`, `verify_client.py`, or `prove_provisioning.py`
default to the same location; `HOMECLOUD_SSH_CONFIG` overrides their config path.
They do not search old checkouts or fall back to a repository-local directory.
The main config includes the sibling `lab_guests.conf` **before** its host
stanzas. `fetch_evidence.py` regenerates only that guest file and verifies guest
key pins, never rewriting the physical host's address or management settings.
Keep these access files private and out of Git.

Verify the independent connection before a deployment rehearsal:

```sh
ssh -F "$HOME/.config/homecloud/host-access/resolvr-box/ssh_config" \
  -o BatchMode=yes hclab-host 'hostname; sudo -n true'
```

## Control API proofs

```
python3 lab/control_runner.py boot --state /private/tmp/hc-boot.json \
  --ssh-config "$HOME/.config/homecloud/host-access/resolvr-box/ssh_config" --cli target/debug/homecloud
python3 lab/control_runner.py resume --state /private/tmp/hc-boot.json \
  --ssh-config "$HOME/.config/homecloud/host-access/resolvr-box/ssh_config" --cli target/debug/homecloud
python3 lab/control_runner.py cleanup --state /private/tmp/hc-boot.json \
  --ssh-config "$HOME/.config/homecloud/host-access/resolvr-box/ssh_config" --cli target/debug/homecloud
```

`resume` waits for the recorded creations; `cleanup` cancels an incomplete creation
or observes its existing deletion, then restores the captured grant. Reviewed
operations still require inspection and an explicit CLI retry. It never deletes
by a broad name prefix, touches managed apps, force-stops guests or releases claims
manually. `cleanup_orphans.py` is a separate local-root operator tool, only for
explicitly selected, deletion-requested proof IDs with verified process identity.

`dns_guest.py` and `persistence_guest.py` refuse to run without the disposable-VM
marker `/etc/homecloud-recovery-proof`. DNS tests use real APT with a guest-local
SERVFAIL responder and restore the resolver. Persistence tests use real OpenZFS
kernel parameters and `update-initramfs`; CPU placement is excluded from this
component test and verified separately on the physical host. Never install this
test boot service on the physical server. Capture the private manifest outside
the repository; commit only reviewed, credential-free evidence summaries.


Run `recovery_runner.py prepare|exercise|cleanup` with the same `--state`,
`--ssh-config`, `--cli` and `--evidence-dir` on each invocation. Prepare creates a
4 GiB/20 GiB test VM plus two small concurrent provisioning fixtures. Exercise
installs the current test modules in the marked VM, kills actual initramfs work
at three checkpoints and reboots only that VM, then injects DNS faults while
APT downloads a real package. CPU placement is explicitly excluded inside this
component fixture. Cleanup deletes the recorded IDs and restores the original
grant. The physical server and permanent devbox are never rebooted by this test.


`memory_runner.py --state … --ssh-config … --cli … --evidence …` reserves the full
lab guest budget and compares two 120-second runs of Homecloud Automatic versus a
custom cap at the same effective 2 GiB ceiling. Memory allocations run only in
manifest-recorded disposable VMs; the root helper rejects permanent VMs and any
VM with managed applications. The existing 4 GiB devbox stays idle. This validates
full admission and bounded disposable load, not every configured byte resident at
once. Failures stop load but retain fixtures for inspection. Cleanup uses the
recorded IDs and tagged dataset, never a broad pool or VM-name deletion.

`startup_soak.py --state … --ssh-config … --cli …` runs seventeen batches of three
new VMs, seventeen stop/start release-and-readmission checks, and six immediate
Create cancellations. It retains failed fixtures and writes a private failure
bundle; `--cleanup` deletes only its recorded IDs through the normal API and
restores the captured grant. `startup_diagnostics.py` captures bounded Incus,
QEMU, cgroup and kernel evidence only for verified, unpaired proof IDs. The optional
`qemu_startup_trace.bt` records process relationships, one-byte syscall lengths
and return values, never buffers. Keep raw captures private. A successful soak
is evidence of the guarded path, not proof of the original orphan's cause.

`launch_reproduction.py --state … --ssh-config … --cli … --cycles 8` adds guest
reboots with a concurrent Start request, stop/start readmission, cancellation after
QEMU reaches Running, and alternate immediate cancellations. Repeated Running
intent must return a completed no-op; agent recovery then reads the guest kernel's
actual VSOCK CID. It stops on the first failure and retains fixtures. `--cleanup`
uses recorded IDs and restores the captured grant after explicit repair if needed.
The campaign has a 90-minute cycle-admission budget and bounded individual RPCs.
It targets the lab's Incus 6.0.5 full-respawn reboot behavior. Newer Incus fast-reboot
semantics require reviewing the expected process-generation change first.

The runner first writes its manifest and prints its owner/prefix, then waits up to
120 seconds for a sibling `<state-stem>.observer-ready` file. Before creating that
gate, start `launch_trace.bt` with bpftrace and `launch_witness.py --owner …
--prefix … --output <private-file> --seconds 5700` as local root observers. Keep
each inside a transient systemd service with RuntimeMaxSec 5800, MemoryMax 512M,
CPUQuota 50%, and a 64-MiB output-file limit. The witness also bounds its own output
to 32 MiB and rejects foreign owners/managed apps. Stop the units after the run;
do not change QEMU, AppArmor, or guest credentials to enable observation.

The trace records exec/fork ancestry, exit status, one-byte syscall return values,
pipe inode/mode, and signals excluding routine SIGUSR1 CPU kicks. It never reads
syscall payloads or argument vectors. Save the relevant slice of the root-owned
CPU-launch audit alongside it. `launch_trace_report.py --trace … --audit …
--output …` attributes events to those launcher PIDs. A background child after a
successful parent exit is normal. A failed second generation must not be confused
with a surviving first generation. Not every child write is necessarily captured;
the report distinguishes matched writes from a parent's confirmed success check.
Run `python3 -m unittest test_control_runner test_launch_trace_report
test_launch_witness` from `lab/` for the focused tool regressions.

`isolated_worker_runner.py prepare|persistence|memory|cleanup` uses a separate
7 GiB/32 GiB disposable VM. Pass the same `--state`, `--ssh-config`, `--cli` and
`--evidence-dir` every time. `prepare --resume` can finish interrupted preparation
after inspection; it does not silently retry a reviewed VM operation. The runner
checks the canonical owner, proof name, empty managed-app list, and immutable VM
ID before every host-side action. It does not restart or load the working devbox.

The persistence phase uses a read-only bind mount on the test VM's configuration,
a full loop-backed ext4 filesystem mounted over its `/boot`, and explicit virtual
hard power-off before/during/after actual initramfs generation. Filesystem faults
must leave the production journal Applying; after removing the fault, boot recovery
must finish persistence before acknowledging it. A per-attempt nonce prevents an
old checkpoint from being mistaken for a new power-loss injection. CPU placement
is excluded from this component fixture.

The memory phase creates a private ZFS pool on a loop block device with verified
direct I/O and nested Incus VMs inside the marked worker. This avoids an extra
Linux file cache on the vdev backing file. `-mem-prealloc` plus initial and periodic
checks of the guest RAM mappings (excluding QEMU overhead) verify full residency. Guest page-touching and bounded storage I/O run for two
10-minute rounds at the same 2 GiB cap, first Automatic and then Custom. This tests
policy paths and sustained health; it is not a performance comparison between
identical limits. A 2 GiB disposable swapfile makes paging observable if the image
has no swap. A final cache reduction checks actual reclamation. Record the nested
distro's Incus/QEMU/ZFS versions: these may differ from the physical lab host.
This validates a fully disposable worker component, not full resident saturation
or abrupt physical power loss on the permanent bare-metal server.


## Software deployment proofs

`deployment_runner.py` uses a manifest-owned managed host and independently
bootstraps an inner client. Supply the reviewed Linux bundle path on the physical
build host and its SHA-256 to `copy`, then run `adopt` only for a pre-receipt worker,
`inner`, `install-check`, `check`, `upgrade`, `kill`, and `bad-start` in sequence.
All commands take the same `--state`, pinned `--ssh-config` and Mac `--cli` as the
managed-host runner; artifact commands also require `--bundle-sha256`.

For storage observability, add `--storage-check` to `upgrade`. Before rollback it
checks an enrolled administrator's real controller snapshot for fresh disk/pool
observations, the boot disk, the ONLINE managed `hcnested` pool, vdev-to-disk
links, and honest SMART unavailability when smartctl is missing. It retains the
private snapshot in the worker and records a credential-free summary in the
manifest. Failure retains the upgraded worker for inspection, not automatic
cleanup. Virtual disks do not prove physical hardware SMART support.

`upgrade` changes an acknowledged grant after its snapshot, then proves software
rollback retains it. `kill` sends SIGKILL to a disposable updater after its first
payload replacement and starts the real guarded service. `bad-start` builds a
fixture daemon that passes `--help` but exits during service startup, then uses
explicit software rollback. They check the same running inner QEMU PID, Incus
PID, endpoint identity and durable proof file throughout. A failure keeps the
worker and manifest for inspection. Cleanup uses the managed-host runner's normal
API deletion; it restores the original outer user grant and removes the owned
custom test volume only after the worker is fully gone.

Never run the guest fault fixture directly on the permanent host. Native Keychain
smoke tests are explicitly ignored in CI and create/delete only a disposable item.
See [deployment](../docs/deployment.md) and [credentials](../docs/credentials.md).

## Shared Nix store proof

The Rust `homecloud-nix-lab` runner creates two clients and an isolated worker
through the authenticated Homecloud API, then uses pinned management SSH for a
host-owned store and read-only virtiofs exports. It preserves a private manifest,
fences unrelated VM process identities, and restores its grant only without
drift. See [the prototype report](../docs/nix-shared-store-prototype.md) for the
exact sequence, expected limitations, host requirements, evidence and teardown.
It is an experimental lab fixture, not an enabled managed VM type.

The follow-up [Fedimint hardening report](../docs/nix-shared-store-hardening.md)
describes the pinned real-project workload, concurrent-build and runtime-retention
probes, physical allocation checks, and the remaining shipping gates.
