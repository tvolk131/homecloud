# Lab operation and recovery

The current lab host is `tommy@192.168.86.204`, Ubuntu Server 26.04, Incus 6.0.5,
OpenZFS 2.4.1, an i7-8700K, about 16 GiB RAM, and a GTX 1070 Ti.
Its existing `hclab` pool is a disposable HDD; the Ubuntu NVMe was retained.
No disk formatting occurs in the Homecloud installer.

## Installation boundary

The host must already have a healthy ZFS pool, working Incus/QEMU, nftables,
`br_netfilter`, PCI tools, a C compiler with static libc, and the pinned Ubuntu
cloud image cached in Incus. Review the hardware policy locally before installing:

```sh
cargo build -p homecloud-daemon -p homecloud-cli
sudo python3 scripts/install_host.py \
  --policy examples/lab-host.json --daemon target/debug/homecloudd
```

**The example is only for this lab.** It allocates cores 4/10 and 5/11 to new VMs,
reserves 0/1/6/7 for housekeeping, and preserves the earlier lab VMs on 2/8 and 3/9.
The managed memory budget is 4 GiB; existing lab allocations and OS/ARC headroom
are outside it. A fresh machine needs its own reviewed topology and capacity file.

Installation changes system/user slice CPU affinity, Incus's QEMU launcher path,
systemd units, a dedicated nftables table and the restricted helper sudo rule.
The explicitly listed disposable legacy VMs may need a graceful restart for
AppArmor changes. Installer backups live under `/var/lib/homecloud/install-backups`;
they are local configuration copies, not an off-host recovery strategy.

Installed files:

- `/etc/homecloud/host.json`: root-owned hardware policy.
- `/etc/homecloud/control.json`: controller capacity and helper location.
- `/etc/homecloud/cpu-policy.tsv`: generated launch allocations.
- `/var/lib/homecloud/control`: private SQLite database and Iroh endpoint secret.
- `/var/lib/homecloud/host`: root-owned CPU/GPU handoff state.
- `/usr/local/libexec/homecloud`: fixed host adapter and QEMU wrapper.
- `/var/log/homecloud-cpu-launch.jsonl`: pre-exec CPU fence evidence.

## Bootstrap and pairing

Run on the host through local privileged authority:

```sh
sudo -u homecloud homecloudd --config /etc/homecloud/control.json \
  bootstrap-admin --name Tommy
```

The JSON output includes a secret single-use pairing code, valid for 20 minutes.
Do not paste it into logs, issues or screenshots. Transfer the public
`/var/lib/homecloud/control/server.json` through the trusted host connection and
verify its endpoint identity, then pair in the Mac app or CLI:

```sh
homecloud enroll --ticket server.json
```

The CLI reads the code from stdin. The current Mac has already been paired.
Bootstrap is permanently closed while an administrator record exists.

## Inspect and repair

```sh
systemctl status homecloudd incus homecloud-host-layout
sudo journalctl -u homecloudd
homecloud status
sudo zpool status hclab
```

An unfinished operation keeps its allocations. Inspect the VM and error before
retrying; do not fix ambiguity by deleting unrelated Incus resources. A `needs_review`
operation can be retried by an administrator with `homecloud retry OPERATION_UUID`.
There is currently no force-stop or automatic destructive recovery API.

For a lost administrator device, use local privilege and the existing user UUID:

```sh
sudo -u homecloud homecloudd --config /etc/homecloud/control.json \
  recover-admin USER_UUID
```

Enroll a fresh device key with that new code. Previously revoked keys stay revoked.
Endpoint-key loss and total database loss are not equivalent to device loss; stop
and inspect before reinitializing either. A replacement endpoint must be verified
and paired explicitly. Backup/restore remains deferred.

## GPU and SSH

GPU attachment may blank the host display. The lab operator explicitly chose SSH
management during handoff. The saved original drivers and console state are restored
on detach. New hardware needs verified IOMMU groups and a supported guest driver;
the lab's Pascal GPU uses the R580 branch with Ubuntu-signed kernel modules.
Direct DKMS installation requires MOK enrollment and is not the default unattended
path. Guest Secure Boot remains enabled.

OpenSSH trust files are generated beside the selected client profile under `ssh/`.
They use immutable VM UUID host aliases. Never bypass a changed-key warning with
`StrictHostKeyChecking=no`; inspect the guest/reimage history first.

The earlier temporary lab bootstrap sudo rule is still present because the proving
and installation work uses privileged host access. Remove it before a coworker
release after establishing the permanent operator workflow; the daemon's own
sudo rule only permits the fixed adapter command.
