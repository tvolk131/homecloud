# Repeatable integration proofs

Use the pinned SSH configuration for the dedicated lab host and the authenticated
Homecloud CLI. Proof VMs have a per-run `hc-proof-<uuid>` prefix, owner ID, stable
request keys and a versioned, fsynced manifest. Failed proofs retain resources for
inspection; a timeout is never permission to free a claim or force power off.
Run only one grant-changing proof at a time.

```
python3 lab/control_runner.py boot --state /private/tmp/hc-boot.json \
  --ssh-config .host-access/ssh_config --cli target/debug/homecloud
python3 lab/control_runner.py resume --state /private/tmp/hc-boot.json \
  --ssh-config .host-access/ssh_config --cli target/debug/homecloud
python3 lab/control_runner.py cleanup --state /private/tmp/hc-boot.json \
  --ssh-config .host-access/ssh_config --cli target/debug/homecloud
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
