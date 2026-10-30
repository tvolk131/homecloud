# Host installation and software updates

Installation is an explicit local-root maintenance operation. A normal software
update has a separate path that stops only `homecloudd`; Incus, QEMU and guest
services continue running. Neither path formats disks.

The current tested baseline is Linux x86-64, Ubuntu 26.04, Incus 6.0.5, OpenZFS
2.4.1 and QEMU 10.2.1. Other versions are refused rather than silently treated as
validated. Extend the checked baseline only after exercising it in a disposable
managed host. This is a deliberately narrow MVP installation contract.

## Fresh installation

Prepare and review the host's pool, image, CPU topology, IOMMU configuration,
network policy and capacity policy locally. The example lab policy is not a
hardware detector or a general installation profile. Required tools include
Incus/QEMU, ZFS, nftables, a working static C toolchain, PCI tools, sudo and
`br_netfilter`; disk cloud-init transport also needs `mkisofs`.

Build the daemon, CLI and Rust observer on the target Linux environment. From a reviewed source
checkout, run the read-only policy/prerequisite check first:

```sh
cargo build --locked -p homecloud-daemon -p homecloud-cli -p homecloud-host
sudo python3 scripts/install_host.py --check \
  --policy /root/reviewed-host.json --daemon target/debug/homecloudd
sudo python3 scripts/install_host.py \
  --policy /root/reviewed-host.json --daemon target/debug/homecloudd
```

The matching CLI and observer default to `homecloud` and
`homecloud-host-observer` beside the daemon; `--cli PATH` and `--observer PATH`
can select them explicitly. Check mode validates binaries, versions, CPU siblings, RAM bounds,
pool health, the cached image, nft syntax and static launcher compilation before
changing host policy or services. Installation writes a software receipt and
root pre-start guards for the controller and host-layout service.

Re-running installation on an existing host requires `--maintenance`. This can
restart Incus and alter CPU/firewall/ARC policy. It is not the ordinary upgrade
path, and an interrupted policy installation needs local operator inspection.
Configuration copies under `install-backups` are not a transactional hardware
policy rollback or a backup of VM data. Fresh installation on the intended final
physical hardware remains a separate acceptance task.

The [native observation cutover](host-adapter.md) requires a reviewed maintenance
installation for older Python-only hosts. It adds the exact observer sudo rule
and expands the guarded deployment file set. Finish recovery of any interrupted
old deployment first. An older updater refuses this changed bundle/engine;
replacing just the daemon leaves the required observer unavailable.

## Build and verify an update

On the reviewed Linux build machine:

```sh
python3 scripts/bundle_host.py --bin-dir target/debug \
  --output /tmp/homecloud-host.tar.gz
sha256sum /tmp/homecloud-host.tar.gz
```

Obtain the bundle and its expected digest through a trusted source. A hash checks
content integrity; it does not authenticate an arbitrary publisher. The bundle
contains the daemon, CLI and observer plus an explicit helper allowlist, format, architecture,
persisted-state compatibility revision and per-file hashes. No profiles,
credentials, database or VM data are included.

On the installed host, substitute the reviewed digest below:

```sh
sudo python3 /usr/local/libexec/homecloud-deploy.py check \
  --bundle /tmp/homecloud-host.tar.gz --sha256 REVIEWED_SHA256
sudo python3 /usr/local/libexec/homecloud-deploy.py upgrade \
  --bundle /tmp/homecloud-host.tar.gz --sha256 REVIEWED_SHA256
```

Preflight checks the complete archive without extracting untrusted paths, rejects
links/duplicates/unknown files, verifies content and Python syntax, runs candidate
binary help commands, checks the installed receipt and service guards, and checks
versions, pool health and staging space. It refuses native launcher, firewall or
deployment-engine changes; those require a reviewed maintenance installation.
User grants, host reserve journals, hardware policy and network policy are not
rewritten by a software update.

## Interruption and rollback

The updater takes a durable deployment lock, stops the controller, and acquires
the host operation lock before replacing any installed helper or binary. It
records private recovery copies and a SQLite snapshot, then commits a complete
matched file set using fsynced replacements. Source caches are invalidated.
It releases the locks before starting the guarded service.

If the updater is killed before commit, the independent root pre-start guard
restores the previous software set before either host-layout or the controller
can run. A concurrent live writer prevents service startup. Corrupt recovery
copies or unexplained installed-software drift block startup for inspection.
Do not bypass the guard to force a mixed installation to run.

Once committed, a failing new service does **not** automatically restore an old
database. Inspect `journalctl -u homecloudd`. To restore the immediately preceding
software set, use the deployment UUID printed by a successful update or recorded
in `/var/lib/homecloud/deployments/installed.json`:

```sh
sudo python3 /usr/local/libexec/homecloud-deploy.py rollback DEPLOYMENT_UUID
```

Rollback is allowed only for the current deployment and the same declared
persisted-state format. It keeps the current database, endpoint identity and
resource claims, including work acknowledged after the checkpoint. The local
SQLite snapshot is recovery evidence, not permission to discard that work.
Incompatible state/schema changes must bump `STATE_FORMAT` and provide a separate
migration/recovery procedure. A matching revision is a developer compatibility
contract, not an automatic proof that arbitrary binaries can be downgraded.

Readiness checks the running executable and a newly written public ticket. An
authenticated client request and preserved guest workload remain the external
acceptance checks. Local deployment copies do not protect against total host or
disk loss; off-host backups are deliberately deferred.

## Adopting a pre-receipt lab installation

This route requires the complete current payload, including the native observer
and its reviewed sudo rule. It cannot convert a Python-only host by adding the
deployment engine alone; use the maintenance cutover described above for that
host. Keep older adoption proofs with their matching release.

This is a one-time explicit local-root step. Retain the exact reviewed source
matching the installed helpers and a separately verified installed daemon hash.
Add the reviewed deployment engine at `scripts/host_deploy.py` in that source
tree, then run it there:

```sh
sudo python3 scripts/host_deploy.py adopt-legacy --source . \
  --expected-daemon-sha256 REVIEWED_INSTALLED_DAEMON_SHA256
```

Adoption verifies helper contents, firewall and a reproducibly compiled native
launcher. It installs the fixed guards and receipt and restarts only Homecloud.
A mismatch requires inspection, not substituting whatever file happens to be
installed into the reviewed source. Interrupted adoption is an explicit
maintenance failure and may require local repair before the service can start.
The temporary broad lab sudo rule remains a release-blocking operational cleanup
until the permanent operator access procedure is established.
