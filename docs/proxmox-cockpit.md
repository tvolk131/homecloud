# Owner-only Proxmox cockpit foundation

This is an opt-in, owner-only cockpit for a different Homecloud product shape:
the owner manages Proxmox through a native Homecloud client over Iroh. It is not
a host migration, complete Proxmox replacement GUI, or coworker self-service plane.
The existing Incus controller remains the default.

## Boundary and responsibilities

```text
Desktop / CLI
    Homecloud inventory types and authenticated Iroh RPC
        Rust service on the Proxmox host
            VirtualizationBackend
                SimulatedVirtualization OR official proxmox-client adapter
                    HTTPS Proxmox API
```

The virtualization boundary lives in the server-side `homecloud-virtualization`
crate. The desktop and shared protocol do not depend on Proxmox SDK types,
authentication, certificates, or upstream API response shapes. The backend can
also reach a separate Proxmox host; co-location is the intended deployment, not
a requirement of the interface. A service account with read access to its token
file is sufficient for this slice; it does not need root or a public API port.

Proxmox owns VM execution and resource configuration. Homecloud exposes observed
nodes, QEMU VMs, LXC containers, templates, and storage capacity. Native Proxmox
VM IDs are not the UUIDs from the old Homecloud controller. Unknown metrics stay
null. Reported VM disk capacity is not guest filesystem usage, reserved backing
space, a ZFS quota, or a durability guarantee.

The boundary exposes inventory and [task-tracked QEMU power operations](proxmox-power.md).
Start, graceful shutdown, and explicit force-stop use Proxmox task identifiers
and durable recovery records, rather than generic command execution. Template
cloning and other mutations are not implemented. ZFS topology,
disk health, dataset policy, and destructive storage actions need their own
carefully scoped interfaces; they are not implemented by this inventory call.

The old `homecloud-host::Backend` has CPU pinning, tenant grants, physical
reservations, and recovery contracts that this design does not implement.
Standalone virtualization mode therefore disables old VM/resource/application
RPCs, legacy SSH forwarding, and host/application/storage reconciliation workers.
It requires an enrolled administrator; no per-user resource grant is needed.
Existing device invitation/revocation remains available, but creating coworker
accounts is deliberately disabled. Simulation is always labeled `simulated`;
a missing real backend never falls back to fake data.

## Run against a deterministic fake

Build the CLI and daemon:

```sh
cargo build -p homecloud-daemon -p homecloud-cli
umask 077
STATE="$(mktemp -d)"
target/debug/homecloudd --state-dir "$STATE" --virtualization simulated bootstrap-admin
target/debug/homecloudd --state-dir "$STATE" --virtualization simulated serve
```

Keep the bootstrap token private. In another terminal, set `STATE` to the same
directory and enroll an isolated client profile. Enter the token at the prompt:

```sh
target/debug/homecloud --profile "$STATE/client.json" enroll --ticket "$STATE/server.json"
target/debug/homecloud --profile "$STATE/client.json" virtualization-inventory
```

The fake includes a running T3-named VM and a stopped template; neither actually
exists or executes code. For custom fixtures, supply
`--virtualization-fixture /absolute/path/inventory.json` when serving. It accepts
the inventory object inside the response, including `"source": "simulated"`,
not the outer RPC response envelope. Tests can replace fake observations in memory.

Standalone mode uses `virtualization.db`; the old controller uses `state.db`.
The daemon refuses to reuse a directory containing the other mode's database.
Always use a separate private state directory and client profile. The current
desktop discovers the server mode before choosing its inventory or legacy UI.

## Official Proxmox adapter

The adapter pins the official `proxmox/proxmox-rs` repository at
`89c1d597a9d15743049eadcb1add380573265904` (`proxmox-client` 1.0.4).
This is the upstream HTTP/authentication client, not a complete typed PVE SDK;
Homecloud maps its resource responses into its own small inventory model.
Native dependencies are optional and Linux-only, keeping the Mac build independent
of them. Review the upstream AGPL license before deciding how to distribute a
combined service. Its dependency tree currently emits a future-incompatibility
warning for `syn` 0.12.15 on the tested Rust toolchain.

On a Linux development machine with a C toolchain, OpenSSL development libraries,
and pkg-config available:

```sh
cargo build -p homecloud-daemon --features proxmox
```

Start with a dedicated audit-only Proxmox API token whose effective permissions cover
the nodes, VMs, and storage to display. A restricted token can return a partial
inventory; an empty list does not establish that the host has no resources.
Keep the secret in a private regular file, not in JSON or command-line arguments:

```json
{
  "endpoint": "https://pve.example:8006/",
  "token_id": "reader@pve!homecloud",
  "token_secret_file": "/private/path/proxmox-token",
  "tls_fingerprint": null
}
```

The endpoint must be HTTPS without embedded credentials or an API path. The
secret file must have no group/other access, for example mode 0600. Normal CA
verification is the default. For a self-signed PVE certificate, an independently
verified SHA-256 fingerprint can be supplied through `tls_fingerprint`. The
upstream fingerprint mode also accepts a normally CA-validated certificate;
do not treat it as exclusive certificate pinning. There is no insecure TLS mode.

Bootstrap and serve with a new private `STATE` directory:

```sh
target/debug/homecloudd --state-dir "$STATE" --virtualization proxmox bootstrap-admin
target/debug/homecloudd --state-dir "$STATE" --virtualization proxmox \
  --proxmox-config /private/path/proxmox.json serve
```

Enroll and query using the same CLI commands as the fake. The service uses
`GET /api2/extjs/cluster/resources`, PVE token authentication, a 15-second request
timeout, and a response-size check. The official client buffers the HTTP body
before Homecloud applies that size check; this is not a streaming memory bound.
Upstream authentication/HTTP errors do not echo response bodies or token values
to clients.

## Native desktop

After pairing the isolated profile using the CLI, launch the desktop against it:

```sh
HOMECLOUD_PROFILE="$STATE/client.json" cargo run -p homecloud-desktop
```

Alternatively, launch with a new private profile path and pair inside the app
using the public server ticket and bootstrap invitation. The desktop asks the
authenticated server for its control-plane mode before opening the old VM-create
outbox or replaying saved work. Virtualization mode opens a separate, owner-only
cockpit: overview/node observations, VMs and templates, guest details, storage
capacity, and client preferences. It never loads or submits legacy VM requests.
Eligible QEMU guests have confirmed power controls and durable operation history;
see [power permissions and recovery](proxmox-power.md). There is no VM creation,
SSH forwarding, or T3 setup action in this view. A T3-looking VM name is not proof
that T3 is installed or connected.

The daemon must support the new `control_plane_mode` RPC, including for existing
managed Incus hosts. Use a matching daemon build with this desktop version.
Unsupported discovery, authentication errors, and unreachable hosts fail closed:
the desktop does not guess a mode or replay saved VM work. Existing installed
server binaries are not upgraded automatically by running this checkout.

Inventory refreshes every five seconds. Failed refreshes retain the last sample
with a persistent stale warning; an observation older than 30 seconds is stale
even without an explicit error. Missing metrics remain "Not reported", not zero.
Empty lists are described as resources visible to the configured API token,
not a claim that no resources exist. A moved or no-longer-visible selected guest
is not treated as a confirmed deletion. Storage capacity is deliberately not
presented as ZFS topology, disk health, backing-space reservation, or a quota.

## Validation and remaining work

The first slice has tests for mapping native identities and nullable metrics,
malformed resources, deterministic fake updates, administrator-only access,
revocation, state-directory separation, legacy RPC/SSH rejection, and real local
Iroh enrollment/inventory. Linux tests exercise the official client against
an HTTP fixture, including the exact token header, endpoint, extjs envelope,
upstream failures, and production configuration checks.

The desktop has 32 focused cockpit tests for mode discovery, discarding legacy
actions, inventory semantics, guarded power controls, confirmation, and lost-RPC
recovery. Fourteen cockpit screenshot baselines cover inventory pages, power
controls, progress/failure, uncertainty, and acknowledgement, including 800×600
layouts. The full 146-test desktop suite and full workspace regression suite pass with the
software renderer:

```sh
ICED_TEST_BACKEND=tiny-skia cargo test --locked --workspace -- --test-threads=1
cargo fmt --all -- --check
```

Earlier fixture-only validation on November 2, 2026 passed the Linux adapter
tests on `resolvr-box` in an isolated build directory with privately unpacked
OpenSSL development files. The daemon also compiled with the real adapter
enabled. That fixture-only run made no service, VM, or storage changes and did
not establish real PVE compatibility.

The subsequent [disposable live validation](proxmox-cockpit-live-validation.md)
passed against PVE 9.2.25 inside a nested VM on the same physical host. The pinned
official client correctly mapped one node, two QEMU guests including a template,
and two directory storages. Both verified-fingerprint and process-local CA trust
worked through authenticated loopback Iroh. Invalid credentials, untrusted TLS,
publicly readable secret files, and unauthorized Homecloud clients were rejected;
the audit-only PVE token's attempted mutation returned HTTP 403. No production
Rust mapper changes were needed. The 26 local lab tests and eight Linux adapter
tests passed; the production daemon and probe compiled.

All disposable resources and private remote builds were removed. Recorded
physical-host baseline fields matched, the controller and CPU policy were
restored, the original VMs remained two running and one stopped, and the physical
pool remained ONLINE. An earlier attempt's unrelated Incus image-cache update
was recorded and left untouched. The physical host still runs Ubuntu/Incus; no
Proxmox service or web GUI was left running. Sanitized observations, source
hashes, failed-attempt history, and cleanup checks are in
`lab/evidence/proxmox-cockpit-live.json`.

This does not establish laptop-to-host relay/NAT access or a live native-desktop
end-to-end flow. Live LXC/multi-node inventory, provisioning/cloning, physical-host
migration, disk enforcement, and host-loss recovery remain unvalidated. Power
operations have their own [live validation](proxmox-power-live-validation.md),
including native task completion, restart tracking, and graceful-failure handling.
The two
fixture storages share a filesystem; their capacities are not independent
physical storage. The live-validation document records the precise scope.

The inventory slice and its live API validation are complete. Task-tracked power
controls are documented and validated separately in [VM power operations](proxmox-power.md).
Physical host installation/migration, destructive storage
operations, and any hard disk-boundary guarantees remain separate work.
