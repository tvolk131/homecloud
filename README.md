# Homecloud

A Rust control plane and native Mac app for a small, trusted Incus/ZFS VM host.
The first working slice includes device enrollment, resource grants, VM lifecycle,
dedicated cores, GPU passthrough, and OpenSSH over Iroh.

**Early implementation, under active lab validation.** See [MVP decisions](docs/mvp.md),
[operations](docs/operations.md), and [validation](docs/validation.md) before adding coworkers.
Backups are deliberately deferred; this host is disposable. See the [current roadmap](docs/roadmap.md) for stabilization and acceptance work.

## Run the Mac app

```sh
sh scripts/package_mac.sh
open target/Homecloud.app
```

The app uses `iced 0.14` and `iced-m3 0.1.0-beta.6` with its official offline
Material Symbols catalog. Rust 1.99 is the currently
tested toolchain. The package includes the `homecloud` OpenSSH transport helper.
Debug symbols and incremental compilation are disabled to keep local builds small.

The current development Mac is already paired with the lab host. The default
paired-host metadata is `~/.config/homecloud/client.json`; Mac device keys live in Keychain. `HOMECLOUD_PROFILE` selects
another profile; `HOMECLOUD_CLI` overrides the SSH helper path.

Use **Open Terminal** for ordinary SSH over Iroh. In **New VM**, select
**T3 Code**, or open an existing VM’s details. Homecloud installs
T3's boot service and native Codex/Claude provider CLIs, then guides browser account
authorization. Sign
into the same T3 Connect account on your Mac to select the backend; guest IPs and
SSH environment setup are unnecessary for that path. See
[managed applications](docs/managed-apps.md) for setup, recovery, and CLI commands.

## CLI

```sh
cargo build --workspace
target/debug/homecloud status
target/debug/homecloud create devbox --memory 2 --cores 1 --disk 20 \
  --ssh-key ~/.ssh/id_ed25519.pub
target/debug/homecloud wait OPERATION_UUID
target/debug/homecloud ssh-config VM_UUID --install
```

Sizes use GiB. The initial image requires at least 10 GiB of disk; each VM also
commits the configured physical disk overhead (currently 2 GiB). VMs use the same
1 GiB RAM minimum with or without GPUs; size memory for the guest image, drivers
and workload. Resize/GPU changes can restart the guest. A full stop releases CPU, RAM and GPU only after confirmed shutdown
and host cleanup; disk remains allocated. Starts atomically reacquire compute.
A busy remembered GPU leaves the VM stopped and offers an explicit CPU-only
start in the GUI, or `homecloud start VM_UUID --without-gpu`. That choice keeps
the GPU preference for the next boot. Internal resize restarts retain claims.

## Development

```sh
cargo test --workspace --exclude homecloud-desktop
cargo fmt --all -- --check
sh scripts/test_ui.sh
```

The native GUI has deterministic screenshot comparisons and interaction checks.
See [interface design and visual testing](docs/interface.md) for baseline review,
actual images, and highlighted diffs.

The transport test uses real Iroh endpoints over explicit loopback addresses. The storage and
allocation tests use disposable SQLite databases. Real QEMU, firewall, ZFS and
GPU behavior requires the Linux lab; a simulator cannot establish those guarantees.

For isolated development, run `homecloudd --simulate --state-dir PRIVATE_DIR
bootstrap-admin`, followed by `serve` with the same options. Pair using the
public `server.json` and the bootstrap code. Simulation is explicitly labeled and
does not run guests or provide SSH.

Never commit `.local/`, endpoint secrets, profiles, invitation codes, private SSH
keys, raw guest logs, or machine-specific known-host credentials. The checked-in
`lab/` files document the earlier proofs; they are not a general-purpose installer.

Client device keys use the native macOS Keychain; see [credential storage](docs/credentials.md).
Use the separate [host software deployment procedure](docs/deployment.md) for upgrades.

Administrators can inspect physical disks, imported ZFS topology and cached SMART
observations in **Hosts → Disks & ZFS**. See [storage observability](docs/storage-observability.md)
for freshness, prerequisites and the read-only/physical-validation boundaries.
The collector is Rust code shipped in the existing host daemon; the privileged
Python adapter only dispatches its fixed local command.
