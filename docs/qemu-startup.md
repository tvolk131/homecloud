# QEMU startup investigation

The October 29 targeted campaign reproduced a guest-reboot/concurrent-Start race
on Incus 6.0.5-8 and QEMU 1:10.2.1+ds-1ubuntu3.3, with the existing Homecloud CPU
launcher and AppArmor policy unchanged. This is a demonstrated failure distinct
from the earlier deleted-instance orphan, whose exact cause remains unproven.

## Captured failure

Three fresh, unpaired 1-GiB VMs provisioned successfully. After a normal stop/start
check, a second VM rebooted from inside its guest while the client requested Start.
Incus launched two QEMU generations approximately 0.34 seconds apart. The first
parent received its readiness byte and exited successfully. The second generation
failed to lock the existing PID file; its parent read EOF and exited with status 1.
The surviving process belonged to the first, successful generation.

The failed start changed `volatile.vsock_id` and the generated QEMU configuration.
Both then agreed with each other but differed from the actual local CID read from
the guest kernel over enrolled, host-key-checked SSH. The guest completed boot and
had working networking, but Incus could no longer reach its agent at the recorded
CID. Homecloud retained all claims and entered review at its bounded agent deadline.
No force kill or automatic repair occurred. After evidence capture, an explicit
graceful stop/start repaired only the proof VM, followed by reviewed retry and normal
API deletion of all three fixtures. Original grants were restored.

The initial trace attributed nine VM launches: eight successful parent readiness
checks and one failed parent with a zero-byte pipe read. Only two child writes were
matched; this is not a claim of complete write-side coverage. Parent exit status
and pipe return values distinguish the failed attempt from the surviving process.

## Source findings

The exact source archives and distribution patch sets were downloaded from Ubuntu
and checked against their `.dsc` SHA-256 entries. The relevant original launch files
match the version-tagged upstream source. Reviewed patches do not change QEMU's
daemonization handshake or Incus's substantive VM launch/operation-lock logic.

QEMU's parent succeeds only after a one-byte zero readiness acknowledgement. A
child startup error closes that pipe, allowing EOF and an unsuccessful parent exit.
The PID-file lock failure seen here is consistent with that expected error path.
It does not establish a broken readiness handshake.

Incus checks startability before acquiring its operation lock, permits Start to
inherit an existing Restart lock, writes the VSOCK allocation/configuration before
QEMU launch, and registers the QEMU-process cleanup hook only after successful
parent completion. These paths make the captured overlap and metadata disagreement
plausible; the trace does not identify every internal Go goroutine or prove which
of those paths caused the original deleted-instance orphan.

Current upstream main also contains related safeguards against duplicate stop
hooks and cleanup while QEMU is still alive. The inspected stable-6.0 start path
still permits Restart-lock inheritance. These source differences are upgrade
research leads, not a verified fix for this lab's incident; no Incus/QEMU package
upgrade was made during the experiment.

Homecloud's launcher moves the process into its assigned cpuset, sets initial
affinity, writes a launch audit, and execs the real QEMU. Its policy/audit descriptors
are closed or close-on-exec; it does not proxy the daemonization pipe. Its failure
exit is 125. No evidence from this run establishes a launcher defect.

## Homecloud correction

Both Start API forms now finish transactionally as a durable no-op if Running is
already desired and compute remains claimed. This preserves authorization checks,
idempotency, observations, existing errors, CPU/RAM/GPU claims, and GPU preferences.
The reconciler receives no new physical-start work. A genuine start after confirmed
shutdown still admits resources normally; reviewed repair remains explicit.

Regressions cover simultaneous requests through independently opened database
connections during a transient stopped observation, durable request replay, error
retention, and the existing CPU-only GPU-consent behavior.

The new metadata witness initially retained SQLite handles between samples. The
replacement explicitly closes every connection and has a 500-sample real-database
regression that retains object references so garbage collection cannot hide a leak.
Its replacement introduces a documented userspace-witness gap; the kernel trace
continued. This affected observation, not the controller's database handles or VMs.

Final post-fix counts and cleanup checks are recorded in [validation](validation.md)
and the reviewed lab evidence. Passing this campaign does not prove the original
orphan's cause or justify automatic force killing on future ambiguous failures.

Sources: [QEMU daemonization](https://github.com/qemu/qemu/blob/v10.2.1/os-posix.c),
[Incus VM launch/reboot](https://github.com/lxc/incus/blob/v6.0.5/internal/server/instance/drivers/driver_qemu.go),
[Incus operation locks](https://github.com/lxc/incus/blob/v6.0.5/internal/server/instance/operationlock/operationlock.go),
[Incus subprocess handling](https://github.com/lxc/incus/blob/v6.0.5/shared/subprocess/proc.go),
[Incus forklimits exec](https://github.com/lxc/incus/blob/v6.0.5/cmd/incusd/main_forklimits.go),
[current main VM lifecycle source](https://github.com/lxc/incus/blob/main/internal/server/instance/drivers/driver_qemu.go),
[stable-6.0 VM lifecycle source](https://github.com/lxc/incus/blob/stable-6.0/internal/server/instance/drivers/driver_qemu.go).
