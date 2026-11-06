# Persistent Proxmox sandbox on resolvr-box

Created November 4, 2026 for interactive experimentation. The physical host
remains Ubuntu/Incus, and the Homecloud controller stays active. This is a
separate, explicitly accounted Incus VM, not yet a normal VM listed in the
Homecloud Mac app.

**Credential setup is still pending:** the supplied password did not meet the
12-character setup minimum, and the replacement private password card timed
out. The root account remains locked; the web interface itself is reachable.

## Open the web interface from the Mac

Start an SSH tunnel in Terminal and leave that Terminal open:

```sh
ssh -F ~/.config/homecloud/host-access/resolvr-box/ssh_config \
  -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 \
  -o ServerAliveCountMax=3 \
  -N -L 127.0.0.1:8006:172.31.246.2:8006 hclab-host
```

Open `https://localhost:8006` in a browser. The browser will warn about the
sandbox's self-signed certificate and the localhost hostname. Proceed only for
this local sandbox connection; do not disable certificate checks globally.

- Username: `root`.
- Realm: **Linux PAM standard authentication** (`pam`).
- Password: the new sandbox password set using a completed private password card,
  **not** the Mac login password used for Keychain approval.

To finish setting the password without another private card, run this in
Terminal and enter a new password of at least 12 characters at the hidden
password prompts:

```sh
ssh -t -F ~/.config/homecloud/host-access/resolvr-box/ssh_config hclab-host \
  sudo -n incus exec hc-94694295416f446e87b070e0756c3a59 \
  --mode=interactive -- passwd root
```

This runs `passwd` inside only the sandbox. It does not change the Mac or
physical-host password, and the password is not placed in shell history.

The SSH command uses the existing pinned management connection and private key.
It requires this Mac to be able to reach the host's management SSH endpoint.
The tunnel listens only on Mac loopback; no router, LAN, or public Proxmox port
is opened. If port 8006 is already in use, first check whether the existing
sandbox tunnel is still working. An alternate local port can be selected by
changing only the first `8006`, then using that port in the browser URL.

A checkout-local convenience script is `.local/proxmox-sandbox/ui.sh`.
Its provisioning scripts and captured observations are private and gitignored.
The persistent host-side scripts live at
`/var/lib/homecloud/proxmox-sandbox/`; no web password is stored in this checkout.

## Resources and isolation

- Incus instance: `hc-94694295416f446e87b070e0756c3a59`.
- Proxmox node hostname: `hc-pve-9469429541`.
- VM memory: **8 GiB**; four shared vCPUs on the existing housekeeping CPUs.
- VM disks: 32 GiB boot disk and a 64 GiB scratch disk.
- Outer ZFS parent:
  `hclab/proxmox-sandbox-94694295416f446e87b070e0756c3a59`, with a 128 GiB quota
  and reservation. Its Incus storage pool is `pve-sandbox`.
- Private uplink: `hcvmpvelab`, subnet `172.31.246.0/24`, VM `172.31.246.2`.
  Existing Homecloud firewall rules isolate this bridge from the physical host,
  LAN, and other guest bridges while permitting public Internet egress.
- No physical disks, GPUs, existing guest disks, or host installation are
  passed through or reformatted.

Homecloud's supported reserve operation adds **8 GiB** to the system reserve,
bringing its total to 12 GiB. The existing 32 GiB user grant is unchanged.
The existing two-core total system CPU reserve, 30 GiB disk floor, and 2 GiB
host ARC limit are unchanged. ZFS accounts for the separate sandbox's bounded
storage outside Homecloud's tenant subtree.

The host policy has an explicitly reviewed `legacy_vms` entry with no exclusive
CPU IDs for this VM. This preserves its housekeeping-only CPU anchor whenever
Homecloud regenerates CPU policy. It does not consume any existing guest's
dedicated CPUs. Original host policy and reserve snapshots are stored privately
beside the host-side scripts.

Do not lower Homecloud's total memory reserve below 12 GiB while this sandbox
exists without separately re-accounting its RAM. Nested performance, especially
disk I/O on the HDD pool, is not representative of bare-metal Proxmox.

## Create test guests

- Choose `sandbox` for VM disks. This is a ZFS pool on the disposable 64 GiB
  virtual scratch disk, not a physical host disk.
- Upload installation ISOs to `local`; keep that boot disk's limited space in
  mind when retaining ISOs and backups.
- Attach guest NICs to `vmbr0`. A private DHCP server supplies
  `10.253.211.100` through `10.253.211.200`, gateway `10.253.211.1`, and public DNS.
  NAT allows Internet access but does not expose nested guests on the LAN.
- Use CPU type `host` for Linux guests to expose the available CPU features.
- Start with small guests, for example 1–2 GiB RAM, and leave RAM for Proxmox
  itself. The entire nested environment shares the outer VM's 8 GiB.

The GUI's guest console works through the same web tunnel. Connecting directly
to an inner guest requires an additional deliberate forwarding arrangement.
This lab is disposable and does not have off-host backups.

## Verified setup

Live checks on November 4, 2026 passed:

- Proxmox manager `9.2.25`, running kernel `7.0.14-28-pve`.
- Graceful outer-VM stop/start, persistent storage, and private DHCP/NAT services.
- A temporary nested QEMU VM ran with an actual KVM VM file descriptor and
  acquired DHCP address `10.253.211.117`. It was removed after validation.
- CPU-policy regeneration preserved the sandbox's housekeeping-only cgroup.
- All three original Incus VMs retained their states, configurations, devices,
  and profiles; Homecloud stayed active and the physical ZFS pool stayed ONLINE.
- The UI returned HTTP 200 through the Mac's loopback SSH tunnel, with its
  certificate validated against the CA obtained over pinned management SSH.
- The no-subscription repository is enabled; the subscription-only repository
  is explicitly disabled. Package audit is empty.

Private detailed results are `.local/proxmox-sandbox/verification.json` and
`/var/lib/homecloud/proxmox-sandbox/verification.json`. Password-based web login
remains unverified until the pending credential step is completed.

## Lifecycle and maintenance

Run from the Mac using the same pinned SSH configuration:

```sh
ssh -F ~/.config/homecloud/host-access/resolvr-box/ssh_config hclab-host \
  sudo -n python3 /var/lib/homecloud/proxmox-sandbox/host.py status

ssh -F ~/.config/homecloud/host-access/resolvr-box/ssh_config hclab-host \
  sudo -n python3 /var/lib/homecloud/proxmox-sandbox/host.py restart
```

`restart` gracefully stops and starts only this tagged sandbox. Shut down nested
guests first when their data matters. Incus autostart is enabled, and the
existing host CPU-layout boot service recreates the sandbox's CPU anchor before
Incus starts. Enabling this policy does not itself test a physical-host reboot.

Do not use the disposable spike runner's cleanup action against this persistent
sandbox. Removal should explicitly check its identity, stop nested workloads,
remove only its tagged Incus/ZFS resources and CPU-policy entry, then release
the added RAM reserve through Homecloud. Do not restore an entire old host
policy over unrelated subsequent edits.
