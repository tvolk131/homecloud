# Homecloud

A Rust control plane and native Mac app for a small, trusted Incus/ZFS VM host.
The first working slice includes device enrollment, resource grants, VM lifecycle,
dedicated cores, GPU passthrough, and OpenSSH over Iroh.

**Early implementation, under active lab validation.** See [MVP decisions](docs/mvp.md),
[operations](docs/operations.md), and [validation](docs/validation.md) before adding coworkers.
Backups are deliberately deferred; this host is disposable.

## Run the Mac app

```sh
sh scripts/package_mac.sh
open target/Homecloud.app
```

The app uses `iced 0.14` and `iced-m3 0.1.0-beta.5`. Rust 1.99 is the currently
tested toolchain. The package includes the `homecloud` OpenSSH transport helper.
Debug symbols and incremental compilation are disabled to keep local builds small.

The current development Mac is already paired with the lab host. The default
private credential is `~/.config/homecloud/client.json`. `HOMECLOUD_PROFILE` selects
another profile; `HOMECLOUD_CLI` overrides the SSH helper path.

Use **Open Terminal** for ordinary SSH. **Set up T3 SSH alias** adds a managed
`Include` to `~/.ssh/config` while preserving existing entries. Add that alias
under T3 Code's **Settings → Environments → Add environment → SSH**. Guest SSH
authentication and strict VM-specific host-key checking remain enabled.

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
commits the configured physical disk overhead (currently 2 GiB). GPU VMs require
at least 4 GiB RAM on the initial implementation. Resize/GPU changes can restart
the guest. Stopped VMs keep their CPU, RAM, disk and GPU commitments.

## Development

```sh
cargo test --workspace --exclude homecloud-desktop
cargo fmt --all -- --check
```

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
