# Validation record

## Automated tests

`cargo test --workspace --exclude homecloud-desktop` covers:

- Quota reductions preserve commitments and independently reject positive growth.
- Concurrent creates cannot both consume the final RAM/core budget.
- Idempotent replay does not allocate twice; reusing a request ID for different intent fails.
- Invitations expire, are single-use, and cannot resurrect revoked device keys.
- Ordinary users cannot inspect another owner's VM or modify grants.
- In-flight operations survive database reopen with allocations held.
- A physical GPU lease is unique and permission revocation is prospective.
- Replaying after an external host effect converges without duplicating the VM.
- Real QUIC/Iroh enrollment, replay denial, revocation, and delivery of SSH authorization errors.

The current tests use SQLite on disk, including real concurrent transactions.
The simulator models external effects for the crash-boundary test; it does not
model a hypervisor, storage guarantees, GPU reset, or firewall behavior.

`cargo clippy --workspace --all-targets -- -D warnings` and Rust formatting are
checked separately. A GitHub Actions workflow is configured for Ubuntu and macOS;
it has not run on GitHub yet. iced-m3's dependency tree currently reports a future
compatibility warning for upstream `block 0.1.6`; project code is warning-free.

## Real host integration

Performed on the Ubuntu/Incus/ZFS lab described in [operations](operations.md):

- Installed an unprivileged controller with a fixed privileged adapter.
- Created a VM through the authenticated Iroh API and observed completion.
- Native iced/iced-m3 Mac app displayed real inventory, allocations and the running VM.
- Standard OpenSSH reached `dev` (UID 1000) through Iroh using enrolled guest host-key trust.
- An intentionally wrong host key was rejected without changing saved trust.
- Guest HTTPS to a public Internet service succeeded. Connections to the bridge's
  host SSH port, LAN router, physical host and earlier lab VM were blocked.
- A second user created an independent VM. Their snapshot omitted other users/VMs,
  their SSH request for the administrator's VM was denied, and cross-VM TCP was blocked.
- Revoking that user's active device disconnected its OpenSSH session in about
  1.24 seconds and denied reconnect. This caught and fixed a blocking-stdin shutdown
  bug in the CLI's one-shot ProxyCommand.
- Deleting that disposable VM released its commitments; the account was disabled.
- Restarting the daemon after persisting a resize intent converged to the requested
  4 GiB size without another VM or another allocation.
- Installed T3 v0.1.2 as the guest's dev user with a persistent systemd user service
  and confirmed it listens on `127.0.0.1:3773`.

GPU integration also passed actual non-root CUDA vector addition (262,144 elements),
cold-start compute, retention of the stopped VM's GPU lease and commitments,
unchanged SSH host-key trust, persistent workspace and T3 service, guest telemetry,
and detach/resume. The signed module reports Canonical as its signer and guest
Secure Boot remains enabled.

The first driver attempt exposed a real provisioning bug: direct driver-package
installation selected DKMS, whose key was not enrolled in the guest. The installer
now selects Ubuntu's precompiled signed modules first. The existing guest was
repaired and the original operation retried successfully with its allocations intact.
See [Ubuntu's driver guidance](https://ubuntu.com/server/docs/how-to/graphics/install-nvidia-drivers/).

No full provider session or off-LAN connectivity result is claimed yet.

## Earlier hardware proofs

The checked-in [proof summary](../lab/evidence/proof-summary.json) records the
pre-implementation storage, CPU and GPU proving work. It is distinct from testing
the new controller. Scripts under `lab/` are tied to that explicitly disposable
host and are not ordinary CI tests.

The important storage counterexample is preserved: per-user parent reservations
failed when root pool `available` reached zero. A controlled scratch-pool test
with a separate unreserved operating floor passed independent-user write pressure,
readback, trim, restart and pool import/export. That establishes a documented
policy requirement, not a blanket promise against arbitrary snapshots or faults.

The CPU proof covered launch, restart, stop/start, Incus restart and host reboot
under simultaneous workloads. Complete SMT groups and pre-exec helper placement
were checked; residual kernel activity is outside a hard real-time guarantee.

## Remaining release checks

- T3 desktop connects to the generated SSH alias and a real owner's provider
  session continues while the Mac is disconnected. Owner login is still required.
- Off-LAN Iroh, sleep/wake, relay fallback and network-change behavior.
- A clean install on different hardware and a reboot after the final installed policy.
- More fault injection around Incus async operations, failed guest shutdown,
  full disks, GPU reset failures, changed keys and externally altered resources.
- Whole-device GPU exclusion under actual concurrent attachment operations.
- Backup/restore when the operator chooses an off-host destination. It remains deferred.
