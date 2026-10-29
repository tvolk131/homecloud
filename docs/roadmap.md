# Current roadmap

This is the current backlog. [Validation](validation.md) retains dated evidence;
older lab summaries describe what was known at the time, not outstanding work.

## Working and validated on the lab

- Native macOS VM/host/user management, enrollment, invitations and revocation.
- Durable transactional CPU/RAM/disk/GPU claims, full-stop release, multi-GPU sets,
  and idempotent creation/recovery. The one-GPU lab does not prove multi-card hardware.
- T3 Connect and Codex/Claude setup, owner-confirmed tasks and reboot/sleep recovery.
- SSH over Iroh, guest/LAN isolation, backed storage and dedicated CPU placement.
- Admin system reserves, backed ARC policy, reviewed RAM-capacity adoption,
  persistent memory warnings and explicit recovery.
- A disposable managed Homecloud host with real nested Incus/ZFS/VM proofs.
- Two-user RAM shortage/recovery and competing starts/grants, with deterministic
  failure tests and directly inspected native screenshot comparisons.

The October 29 checkpoint passed 203 Mac Rust tests, 118 relevant Linux Rust tests,
121 Python tests (two Mac platform skips), and 133 native screenshot comparisons.
See `lab/evidence/memory-recovery.json` for that lab state and boundaries.
The stabilization pass adds Keychain and software-deployment coverage: 211 Mac
Rust tests, 125 Linux Rust tests and 136 Python tests, plus native/live proofs.

## Stabilization in progress

- [x] Publish a reviewed GitHub checkpoint and keep CI exercising the tested paths.
- [x] Store Mac device credentials in Keychain; preserve identity during migration
  and fail clearly if the credential store is unavailable.
- [x] Separate host installation from normal upgrades, add preflight/version checks,
  and validate atomic deployment plus rollback in a disposable managed host.
  Live interruption/rollback proofs and the approved physical-host deployment
  passed; the permanent T3 devbox stayed running.
- [ ] Replace lab-only privilege/setup assumptions with an operator installation
  procedure before coworker release. Keep local-console recovery available.

## Remaining acceptance work

- [ ] Clean install and realistic resident-memory/VM-density tests on the intended
  physical hardware. Nested tests do not establish physical-host density.
- [ ] Repeat mobile automatic/relay networking with monitored hotspot routing.
  This remains deferred at the operator's request; a prior successful full run
  could not be attributed entirely to the hotspot after the phone died.
- [ ] Validate actual multiple-GPU hardware and its device-reset behavior when
  suitable hardware is available.
- [ ] Capture actionable Incus/QEMU incident evidence automatically. The earlier
  isolated Incus SIGKILL and deleted-instance QEMU orphan causes remain unproven;
  lifecycle safeguards are tested independently of those diagnoses.

## Later or explicitly deferred

- Full disk/SMART inventory, persistent alert history and idle-GPU advisories.
- Supported T3 Connect account identity via an upstream status field.
- Off-host backups and restore drills: deliberately deferred while host/data loss
  is acceptable. Local installation snapshots are not off-host VM-data backups.
- Fleet aggregation/migration, user thin provisioning/snapshots and shared CPUs.
- Sidebar icons now use the official offline Material Symbols catalog through
  iced-m3 beta.6; its upstream integration is merged.
