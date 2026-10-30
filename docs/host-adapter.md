# Rust host adapter migration

Inventory and VM observation always run through the Rust
`homecloud-host-observer` executable. Their Python implementations have been
removed. There is no opt-in setting or Python fallback for these requests.
A failed native observation remains visible while resource claims stay held.

The daemon invokes the root-owned observer at
`/usr/local/libexec/homecloud/homecloud-host-observer` through an exact sudo rule
with no arguments. The observer requires Linux and root, reads one bounded JSON
request from stdin, and accepts only `inventory` and `observe`. Host paths and
external commands are fixed. Command output is bounded while reading, including
optional GPU telemetry from a guest. It expects `/usr/bin/incus`, `/usr/bin/lspci`,
`/usr/sbin/zfs` and `/usr/sbin/zpool` on the validated Ubuntu host.

The observer reads the root-owned pool policy and private reserve journal to
include additional disk reserves in the reported operating floor. It observes
IOMMU/GPU readiness, pool health, VM ownership/configuration drift and setup
progress. It does not replay or enforce policy. Full configuration validation,
locks, journal recovery, diagnostics, applications and every mutation still run
through the remaining Python adapter.

Existing `host_script` configuration continues to select that remaining adapter.
An explicit `host_command` object can select another primary executable and
literal arguments. Neither setting changes the mandatory native observation
route. These commands come from local daemon configuration, never network
requests. The daemon constructs no shell command.

## Installation and the cutover boundary

Build all three binaries on the target Linux environment:

```sh
cargo build --locked -p homecloud-daemon -p homecloud-cli -p homecloud-host
sudo python3 scripts/install_host.py --check \
  --policy /root/reviewed-host.json --daemon target/debug/homecloudd
```

The installer requires the matching observer beside the daemon, or supplied with
`--observer PATH`. Its preflight checks the executable before persistent changes.
Installation places the observer in the fixed libexec path and installs its
no-arguments sudo rule alongside the remaining Python rule. No extra controller
configuration enables the observer.

An existing Python-only installation needs a reviewed `--maintenance`
installation from this release's source tree after finishing any interrupted
software deployment. This adds the new privilege rule, deployment engine and
three-binary receipt. Maintenance can restart Incus; validate on a disposable
host first and use the documented maintenance procedure for working hosts.
The older updater deliberately refuses the changed bundle/engine boundary.
Deploying just the new daemon is insufficient.

Subsequent ordinary bundles include the observer's content hash and executable.
Preflight requires it, checks its architecture and runs its `--help`. The daemon
and observer are checkpointed, replaced, verified and restored as one file set.
Missing or modified observer bytes block the startup guard. Software rollback
retains acknowledged database work. Moving back across the original Python-only
installation boundary is also a maintenance transition, rather than an ordinary
three-binary software rollback. See [deployment](deployment.md).

## Validation and subsequent slices

Run `cargo test --locked -p homecloud-host -p homecloud-daemon`. Rust regressions
use recorded outputs captured from the previous Python adapter with identical
private host files and fake command responses. The fixtures preserve its wire
behavior without retaining another runnable observer. Tests cover drift, GPU
claims, missing VMs, unavailable services, setup progress, command limits and
routing failures. Deployment fault tests include observer interruption recovery,
rollback, missing binaries and pre-cutover receipts.

These checks do not establish real Incus/ZFS or hardware behavior. Repeat the
relevant disposable-host lifecycle and resource-policy proofs before deployment.
Port lifecycle and resource mutations with their failure tests in subsequent
cutovers, followed by guest jobs and installation/deployment tooling. Retire
each corresponding Python implementation as its subsystem moves to Rust.
