# Rust host adapter migration

The daemon can execute an operator-configured host command using the existing
`HostRequest`/`HostResponse` JSON protocol. Existing `control.json` files keep
using `/usr/bin/python3` with their `host_script` path and retain the existing
sudo behavior. Commands and arguments come from local daemon configuration;
network requests cannot choose them. No shell is used to construct the command.

`homecloud-host-observer` is the first native migration slice. It implements
`inventory` and `observe`, including IOMMU/GPU readiness, pool health, VM ownership
and configuration drift, and setup progress. It rejects all other request kinds.
Its paths and external commands are fixed, it requires root on Linux, and it
accepts one bounded JSON request on stdin with no command-line arguments.
It expects `/usr/bin/incus`, `/usr/bin/lspci`, `/usr/sbin/zfs` and
`/usr/sbin/zpool` on the validated Ubuntu host.

The observer reads the root-owned pool policy and private reserve journal to
include additional disk reserves in the reported operating floor. Observation
does not replay or enforce policy. Full configuration validation, locks, journal
recovery and every mutation remain in the primary Python adapter. This allows
read-only inspection without moving enforcement into a second implementation.

## Disposable-host opt-in

This first slice is for explicit lab validation. The standard installer, update
bundle and deployment receipt do not install or manage the observer yet. Build
and install the matching binary separately on the disposable Linux host:

```sh
cargo build --locked -p homecloud-host --bin homecloud-host-observer
sudo install -o root -g root -m 0755 target/debug/homecloud-host-observer \
  /usr/local/libexec/homecloud-host-observer
```

Keep the existing Python sudo rule. Use `visudo` to add a separate
`/etc/sudoers.d/homecloud-observer` rule allowing the observer with no arguments:

```sudoers
homecloud ALL=(root) NOPASSWD: /usr/local/libexec/homecloud-host-observer ""
```

Add this field alongside the existing fields in `/etc/homecloud/control.json`:

```json
"host_observer": {
  "program": "/usr/local/libexec/homecloud-host-observer"
}
```

Restart only `homecloudd`. Inventory and VM observation now use Rust; all other
operations go directly to the existing primary adapter. Native errors remain
visible rather than falling back to Python. Remove `host_observer` and restart
the daemon to return all requests to the primary adapter. Keep the separately
installed observer matched to the daemon during this experiment.

For a later complete adapter replacement, `host_command` selects its executable
and optional literal arguments. It overrides the legacy Python command; it does
not change the read-only routing of `host_observer`. Do not point `host_command`
at this observer: it cannot execute mutations.

## Validation and subsequent slices

Run `cargo test --locked -p homecloud-host -p homecloud-daemon`. Compatibility
tests give Rust and Python identical private host files and fake Incus/system
responses. They cover inventory and observation, ownership/configuration drift,
GPU claims, missing VMs, unavailable services, and routing failures. These tests
do not execute real Incus/ZFS operations or establish hardware behavior.

Before default activation, compare both observers on a disposable managed host
and repeat the relevant lifecycle and resource-policy proofs. Then integrate
the native executable into the matched deployment/rollback file set and retire
the duplicate Python observation functions. Port lifecycle and resource mutations
with their failure tests in subsequent slices, followed by guest jobs and
installation/deployment tooling. Audit historical lab scripts separately.
