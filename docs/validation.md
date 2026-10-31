# Validation record

## Current acceptance status — October 31

- **Passed:** resource-claim/hardware lifecycle proofs, dedicated-core placement,
  storage backing policy, isolation, Iroh authorization and SSH access.
- **Passed:** owner-confirmed T3, Codex and Claude use; authenticated VM reboot
  recovery; existing provider-thread follow-ups without new sign-in; Mac internet
  disconnect/reconnect and actual sleep/wake while work continued.
- **Passed:** two-user nested RAM shortage/recovery, explicit ARC repair,
  concurrent starts/grants, owner-scoped health and persistent GUI guidance.
  See the two-user campaign below.
- **Passed earlier:** relay-only hotspot control, overlapping SSH and SFTP.
- **Passed:** authenticated Incus VM telemetry through controller snapshots;
  actual guest root filesystem accounting, metrics-only TLS permissions, and
  native GUI automated acceptance. Reopened Mac GUI live-data acceptance still
  needs normal Keychain approval; see the October 31 metrics record below.
- **Deferred repeat:** the October 28 automatic/relay comparison completed its
  operations, but the phone died and the Mac rejoined home Wi-Fi at an unknown
  time. Full runs and late fast samples do not count as hotspot acceptance. The
  next run must use the network preflight/monitoring guard.
- **Still unverified:** a clean install on different hardware and broader fault
  cases. Off-host backup/restore remains deliberately deferred.

The detailed sections below retain the history of what was known at each stage.
The [qualified hotspot record](../lab/evidence/hotspot-20261028.json) distinguishes
operation success from verified hotspot routing. Networking work is deferred at
the operator's request while UI cleanup continues.

## Automated tests

The October 27, 2026 run passed:

- macOS: `cargo test --locked --workspace` (70 Rust tests, including the native
  iced screenshot suite with 58 baselines).
- Linux: `cargo test --locked --workspace --exclude homecloud-desktop`
  (46 Rust tests).
- Forty-five Python host/guest-adapter cases, also invoked by the Rust host test wrapper.
- Clippy with `--all-targets -- -D warnings` on both systems, Rust formatting,
  Python compilation and `git diff --check`.

Tests use real SQLite files and independent concurrent connections. They cover
quota/grant races, rollback, idempotency, invitation consumption, revocation,
permissions, stopped-VM admission, unique CPU/GPU leases, retained resize backing,
stale completions and controller process exclusion. Transport tests use real
QUIC/Iroh. Adapter mocks verify shutdown confirmation, cleanup ordering, remaining
host-process refusal and retained claims after a GPU handoff failure.

The native GUI baselines were inspected directly during the interface work. A
GitHub Actions workflow exists for Ubuntu and macOS; no GitHub CI result is claimed.
iced-m3's dependency tree reports an upstream `block 0.1.6` future-compatibility
warning; project code is warning-free.

## Real controller and hardware proofs

The [lifecycle evidence](../lab/evidence/control-plane-lifecycle.json) records the
October 27 run against the Ubuntu/Incus/ZFS host described in
[operations](operations.md). The controller, CLI and privileged adapter were
upgraded together without resetting users, endpoint keys, the database or the
existing devbox's disk/SSH identity.

The following passed against real Incus VMs and the GTX 1070 Ti:

- Full stop: Incus shutdown, GPU detach/host-driver restoration and CPU launcher
  cleanup finish before compute claims are released. Only disk remains charged;
  the preferred GPU is remembered without holding it.
- CPU-only fallback and later GPU reacquisition preserve workspace files, pinned
  SSH trust and the T3 service. Repeated GPU boots perform non-root CUDA vector
  addition on 262,144 elements with the correct result.
- Simultaneous starts by two owners produce one GPU owner. The losing transaction
  rolls back every new CPU/RAM/GPU claim; explicit CPU-only fallback receives a
  disjoint complete SMT group. Concurrent attachment requests from two running
  owners also produce exactly one physical owner.
- GPU permission revocation leaves an existing attachment running. Starting an
  already-running CPU-only VM does not silently reacquire its preferred GPU.
- Lowered RAM/CPU quotas retain existing commitments and prevent their backing
  from being issued again. A disk grant reduction from 96 to 16 GiB retains the
  real 26 GiB ZFS reservation, rejects disk growth and overissue to another owner,
  and permits disk-neutral stop/start.
- Injected failure after physical shutdown/GPU removal retains CPU/RAM/GPU claims
  while host-driver cleanup is incomplete. A competing start is refused. Restoring
  the production adapter and explicitly retrying completes cleanup and releases
  the claims.
- Killing the controller after Incus start but before database completion replays
  the same operation after systemd restart, without another VM or duplicate claims.
  A second live controller is refused by the process lock.
- A running GPU VM's internal disk-resize restart retains all compute claims,
  including while Incus reports it stopped (305 sampled observations).
- Unattended host reboot restores the controller, Incus, CPU policy, running GPU
  VM, CUDA, disk data, SSH trust and loopback-only T3 listener. A deliberately
  stopped VM remains stopped. ZFS reports the pool healthy afterward.
- After reboot, vCPU threads are confined to their assigned SMT siblings and
  userspace QEMU helpers to housekeeping CPUs. VM kernel workers remain within
  that VM's cgroup CPU set; Linux's NX huge-page recovery worker was observed on
  its own VM's dedicated core. This is exclusive VM placement, not hard real-time
  isolation from kernel maintenance. See the upstream Linux
  [worker flag](https://github.com/torvalds/linux/blob/v7.0/include/linux/sched.h)
  and [KVM worker creation](https://github.com/torvalds/linux/blob/v7.0/arch/x86/kvm/mmu/mmu.c).

The first fresh T3 VM exposed a missing runtime dependency: the prebuilt T3 binary
requires `libatomic.so.1`, absent from the minimal Ubuntu image. T3-enabled cloud-init
now installs `libatomic1`; a regression case and a fresh unattended provision passed.
T3 v0.1.2 runs as `dev` through a persistent systemd user service, listening only on
`127.0.0.1:3773`. No provider credentials are supplied during provisioning.

Cleanup also caught a lifecycle guard bug: a fully stopped disk-only VM was refused
for deletion because its desired state had become `deleted` while it held no CPU
claim. The adapter now permits that non-running state; the regression case passed
on Mac/Linux and the original reviewed deletion completed on the real host.

The earlier GPU driver prototype exposed a separate DKMS/MOK problem. The adapter
selects Ubuntu's precompiled signed modules first; guest Secure Boot stays enabled.
See [Ubuntu's driver guidance](https://ubuntu.com/server/docs/how-to/graphics/install-nvidia-drivers/).

## Networking and access

- Public guest HTTPS succeeded while connections to the host bridge, host LAN
  address, Comcast router and another live VM's SSH port were blocked. Both guests
  were reachable from the host as a positive control; nftables drop counters rose.
- An ordinary user's inventory omitted other owners, and their SSH request for
  the administrator's VM was denied.
- OpenSSH SFTP transferred and read back data through Iroh. A deliberately wrong
  guest host key was rejected without modifying saved trust.
- An earlier live device-revocation test disconnected OpenSSH in about 1.24 seconds
  and denied reconnect; it caught a CLI blocking-stdin shutdown bug that was fixed.
- The relay probe succeeded with every direct IP transport disabled: authenticated
  control RPC and an authorized guest SSH banner both traversed an external relay.
  This isolates the relay transport but is not a substitute for a Mac off-LAN test.

Reproduce the relay diagnostic with a private enrolled client profile and owned VM:

```sh
cargo run --locked -p homecloud-client --example relay_probe -- /private/client.json VM_UUID
```

The probe performs authorization and reads the guest SSH banner. It does not log
into the guest. Full guest authentication and SFTP were tested separately.

## Transport identity and Mac checks

The first phone-hotspot run reached control RPC and guest SSH; its SFTP attempt
timed out, then succeeded on retry. Inspection found that the GUI and every
ProxyCommand reused the enrolled device key as their transport endpoint identity.
Upstream Iroh [explicitly replaces relay delivery when that identity connects again](https://github.com/n0-computer/iroh/blob/main/iroh-relay/src/protos/relay.rs).
That is a concrete concurrency flaw; the initial timeout alone does not isolate
it from other mobile-network delays.

Protocol v2 gives each process a fresh endpoint. The existing device signs a
domain-separated [TLS exporter](https://docs.rs/iroh/1.4.1/iroh/endpoint/struct.Connection.html#method.export_keying_material),
with the negotiated ALPN as context. The controller verifies that connection-bound
proof before dispatch and uses the stable device ID for authorization and tunnel
revocation. No database or pairing reset is needed. Tests verify distinct transport
IDs for simultaneous clients, shared device authorization, revocation of both,
forgery denial and rejection of a proof replayed on another real TLS connection.
The matched v2 deployment preserved pairing and the running devbox; forced-relay
control and authorized SSH banner checks passed again.

The operator-assisted baseline sleep/wake recovered control, pinned guest SSH,
SFTP and T3 readiness. The Mac automatically rejoined home Wi-Fi after waking
(gateway `192.168.86.1`), so this was also network-change recovery, not a claim of
staying on the hotspot through sleep. Relay-only mode then passed on the phone hotspot with the GUI open, two overlapping
90-second guest SSH sessions, additional control requests and SFTP; the access
checks took 8.46 seconds. Normal mode completed control and guest readiness slowly,
but the test's 90-second hold expired before SFTP overlap. Earlier normal-mode
attempts also hit a 12-second SSH startup deadline and one peer-close error.
Normal-mode mobile performance/reliability remains open; this is not a claim that
both modes passed. The fixed five-second server grace was investigated but not
changed: a local 456 KB flow-controlled response read after six seconds passed.
The [lifecycle evidence](../lab/evidence/control-plane-lifecycle.json) records the
results and separates hardware-proof hashes from the final source manifest.

## Earlier storage proofs

The earlier [proof summary](../lab/evidence/proof-summary.json) records the storage,
CPU and GPU proving work before the controller existed. Scripts under `lab/` are
tied to that disposable host and are not ordinary CI tests.

Per-user parent reservations failed when root pool `available` reached zero. A
controlled scratch-pool test with a separate unreserved operating floor passed
independent-user write pressure, readback, trim, restart and pool import/export.
This establishes a policy requirement, not a promise against arbitrary snapshots,
foreign host writes or hardware faults. User thin provisioning and snapshots remain
deferred.

## Remaining release checks

- Complete normal-mode mobile reliability diagnosis; the explicit relay-only
  path passed hotspot access. Validate final provider-session continuity across
  sleep/reconnect; the earlier baseline already recovered after sleep/network change.
- T3 Connect account approval and a real owner's provider session continue through
  disconnect/reconnect. Owner login is required; the previous SSH-environment
  onboarding stalled on the poor hotspot and is no longer the preferred T3 path.
- Clean install on different hardware, plus broader package/kernel/GPU coverage.
- Further fault injection for failed shutdown, full disks, actual GPU reset faults,
  changed keys and externally altered resources. The injected cleanup failure
  proves claim retention; it does not simulate an electrically wedged GPU.
- Backup/restore when the operator chooses an off-host destination. This is deferred.


## Managed-application recovery

The new store tests verify atomic per-VM application selection, concurrent request
coalescing, ownership checks, durable operation recovery, and exclusion of browser
challenges from host database records. Six guest tests cover the actual CLI version
prefix, challenge parsing/lifetime, surviving controller reconnection, reboot/dead
job interruption, saved-credential recovery, committed success, and revoked state.
The GUI suite adds waiting, interrupted, connected, stopped, and small-window T3
screens, plus action-targeting and expired-link checks. Actual rendered screenshots
were directly inspected before updating baselines.

On the real devbox, the managed installer completed. A controller restart preserved
the running authorization job and identical valid challenge. Rebooting the VM while
unpaired cleared that challenge, marked the operation interrupted, and automatically
started T3's persistent service. No authorization URL/code was copied into evidence.
The owner subsequently approved T3 Connect and reported successful Mac environment
selection. The paired-VM reboot proof passed: T3 started automatically and became
remotely reachable in 71.11 seconds, using the same saved account/relay configuration
and setup operation without a new authorization link. A real AI-provider task and
provider-session continuity still require owner confirmation.

A fresh 2 GiB VM selected T3 at creation and completed both infrastructure and
application installation independently. T3 0.1.2 was running with account setup
still required. The proof VM was deleted and all its allocations released; the
original devbox was restored CPU-only. Curated evidence is in
[`managed-applications.json`](../lab/evidence/managed-applications.json).

The owner-assisted run exposed one status race: a guest setup job had succeeded
while its host operation remained interrupted. The store now accepts confirmed
success for that same setup job on a VM that has not been deleted. A regression
checks unchanged resource claims, rejection of stale running reports reopening
completed work, and refusal to promote interrupted setup after VM deletion.
The corrected status was verified on the real paired devbox before its reboot.


## Provider bootstrap

The owner-assisted provider task exposed that installing T3 alone does not install
its external provider CLIs. The T3 bundle now includes native Codex 0.165.0 and
Claude Code 2.1.308. T3 0.1.2's bundled manifest supports Codex >=0.156.0 (recommends
>=0.159.0) and Claude >=2.1.280. Initial artifacts are pinned and SHA-256 verified.
No Node runtime, provider credentials, or account configuration is copied from the Mac.

The Ubuntu adapter suite runs 45 cases. On this Mac's Python 3.9, two extraction
cases are skipped because tarfile data filters require modern Python; both execute
on the supported Ubuntu guest. Tests cover changed download checksums, package
traversal, required Codex sidecars, preserving compatible/broken existing installs,
missing providers after successful pairing, and service PATH activation without
restarting an initial service twice or replacing owner state. The GUI suite has
51 baselines, including provider-missing and provider-installed states.

The real repair installed both provider binaries. A process-environment probe
confirmed discovery through the actual T3 service PATH and the Codex app-server
entrypoint, without issuing a model request. T3 account pairing was retained.

After repairing the service activation path, a second paired reboot passed in
78.90 seconds. Both provider binaries remained discoverable through the real
T3 service process PATH, `codex app-server --help` succeeded, and native T3
reachability returned without another account authorization. The owner subsequently confirmed Codex sign-in through Homecloud and working
T3 use in the devbox; see the Codex sign-in acceptance result below.


## Codex sign-in through Homecloud

The named setup-action API serializes requests per application and validates VM
ownership for provider sign-in. Regression tests cover cross-action idempotency,
concurrent requests, owner-only snapshots, separate T3/Codex success, interrupted
jobs and stripping challenges from storage. The JSONL adapter is tested against
real subprocess fixtures for successful/denied/unsaved completion, wrong login IDs,
existing accounts, malformed/oversized frames, deadlines and secret-bearing errors.
The installed Codex 0.165.0 schema/handshake confirms device-code support and
initially reported the guest account as signed out. Browser approval and
functional T3 use were subsequently confirmed by the owner.


Real-lab Codex checks passed: an unapproved device-code login survived a controller
restart with the same guest unit/challenge; its challenge stayed out of the host
DB; T3's service PID did not change. A subsequent VM reboot cleared the incomplete
Codex challenge and interrupted its operation without automatically generating a
replacement or losing T3 pairing. The normal T3 connection and both provider
binaries were checked again afterward. The Mac app was rebuilt and reopened;
native dialog click automation returned `noWindowsAvailable`, so browser approval
is handed to the owner through manual Homecloud navigation. Rendering/interaction
fixtures and direct screenshot inspection cover the dialog itself.


Owner acceptance: the owner reported that Codex sign-in through Homecloud worked
and T3 was usable in the devbox. This confirms the owner-driven sign-in and
functional T3 flow. A specific task artifact was not independently inspected;
authenticated provider-session continuity across a later reboot or network/sleep
change remains a follow-up check. No further workload interruption was performed.


## Shared setup screen and Claude sign-in

The Claude 2.1.308 anonymous spike used a disposable guest HOME and observed its
allowlisted browser URL and manual-code prompt. No owner credential store or T3
session was changed by that spike. Subprocess/FIFO tests cover successful code
return to the same process, incorrect code, expiry, existing-account retention,
control-character rejection, wrong/finished operations and non-FIFO destinations.
Guest recovery tests distinguish Claude authentication from T3 pairing. Server
checks cover current ownership, operation ID/phase, challenge expiry and code
redaction. GUI tests cover immediate feedback, an accepted operation surviving an
older snapshot, cleared code input and visible verification before the next poll.
All new screen states are rendered with the actual iced UI and inspected directly.
Owner browser approval and a real Claude task remain an acceptance handoff.


The initial live Claude check appeared to expose duplicated OAuth parameters.
An owner-reported desktop rejection traced this to OSC 8 terminal hyperlinks:
the old parser joined a hidden target and its visible label with BEL. Accepting
identical duplicate values had only treated a symptom. The parser now handles
whole OSC frames (BEL or ST), ignores incomplete frames and excludes controls.
Shared fixtures cover guest extraction and the actual desktop browser guard;
a subprocess/FIFO test covers the hyperlinked CLI output through code delivery.
The desktop still rejects HTTP and control-bearing URLs.

The parser fix was deployed without restarting the controller, VM or T3. A fresh
real Claude 2.1.308 login yielded exactly one HTTPS URL, zero control characters
and one value per OAuth query parameter. Its guest parser checksum matched the
installed host module; the challenge stayed out of the controller database.
T3's process and existing Codex sign-in were preserved. The rebuilt desktop app
was reopened. At that stage, owner browser approval and a real Claude task were pending; both were subsequently confirmed, as recorded below.

The earlier real login reached its private FIFO/browser challenge. A controller
restart preserved the same job, URL and FIFO; stopping only that unapproved login
unit cleared its challenge and required an explicit retry. T3 kept its process
and pairing, and Codex remained signed in. No VM/T3 workload restart or owner
credential replacement was performed. The rebuilt Mac app was reopened; native
click automation still returned `noWindowsAvailable`, so final browser approval
and Claude task validation are handed to the owner through manual navigation.

A final regression check ensures a completed provider login invalidates any cached
signed-out result taken before it completed. Post-deployment observations still
reported T3 connected and Codex signed in; the owner subsequently completed Claude approval and a real task.


## Unified workspace details

VM management now has one Details destination with pinned lifecycle/terminal
controls and Overview/Settings tabs. The duplicate T3 settings route and generic
Manage modal were removed. T3 connection and provider accounts share one compact
card, expanding only the active sign-in flow. Completion gets an explicit notice.

The 24 desktop tests include actual click/navigation messages for both Details
entry points, header actions targeting the selected VM, tab changes preserving
pending setup while clearing unsubmitted codes, successful sign-in feedback,
separate delete/GPU confirmations, and visible controls/accounts at 800×600.
All 58 native iced screenshot cases pass; new details/settings, ready, long-name
and scrolled provider/settings renders were opened for direct visual inspection.
No server deployment, VM restart or account change is needed for this UI update.
The owner confirmed Claude sign-in succeeded before this cleanup; a Claude model
task was not part of these UI checks.


## Authenticated provider reboot recovery

On October 28 the owner confirmed a real Claude thread worked. With both provider
threads idle, one graceful Incus restart rebooted only the devbox. Fresh probes
against the installed provider CLIs reported Codex and Claude signed in before
and after, using the actual T3 service PATH. T3 auto-started, retained its account
and relay configuration, listened only on loopback, and became remotely reachable.
All recovery assertions completed in 69.05 seconds.

The guest boot ID changed while machine identity, pinned SSH key, resource claims
and software versions remained unchanged. Both persisted T3 thread identifiers and
all 14 existing message-record identifiers remained present; no message bodies
were inspected. Three bounded proof-workspace files, including the reboot
sentinels, matched their prior hashes. Existing SSH-over-Iroh access also succeeded
as dev with strict host-key checking. The controller reported healthy VM and
application observations, with no new setup operation or authorization challenge.
See [recovery evidence](../lab/evidence/provider-reboot-recovery.json).

The owner confirmed follow-up requests through both existing provider threads worked without reauthorization. The reboot acceptance check is complete.
The owner subsequently turned the Mac internet connection off and restored it, and confirmed the workflow worked and thread replies appeared. This passes provider-workflow disconnect/reconnect acceptance by owner report. That check did not involve sleep; the separate actual Mac sleep/wake check was subsequently completed, as recorded below. See [disconnect evidence](../lab/evidence/provider-disconnect-recovery.json).


## Real provider workflow acceptance complete

The owner subsequently completed the actual Mac sleep/wake waiting-task and
follow-up check and reported success. Together with the successful internet
disconnect/reconnect check and both existing Claude/Codex threads responding
without reauthorization after the verified VM reboot, this completes the real
provider-workflow acceptance checks. Sleep timing and the provider replies are
owner-reported rather than independently instrumented; see
[sleep evidence](../lab/evidence/provider-sleep-recovery.json). Automatic-mode
hotspot reliability remains a separate unresolved networking item.


## October 28 hotspot retest and network attribution

Initial authenticated control requests succeeded in automatic and relay-only
modes, with phone-hotspot routing confirmed. Subsequent controlled-session runs
completed control requests, two overlapping SSH sessions, 64 KiB SFTP readback
and continued session use in both modes. However, the phone later died and the
Mac automatically rejoined home Wi-Fi. The exact handover time was not captured,
so those full runs do not establish continuous hotspot acceptance. The later
subsecond persistent-client samples include a private LAN path; they are excluded
from hotspot latency conclusions. The owner's prompt terminal success is also
network-ambiguous. See [qualified evidence](../lab/evidence/hotspot-20261028.json).

The old fixed 90-second SSH holds could expire before a slow transfer began,
causing a harness failure unrelated to an SSH disconnect. The replacement holds
sessions through explicit stdin commands and records payload delivery separately
from process shutdown. It now requires the expected gateway before connecting,
checks gateway/interface once a second and invalidates a route change, with a
separate artifact directory per run. Its wrong-network negative control refused
to send requests on home Wi-Fi. A guarded live hotspot repeat remains pending.

The read-only `connection_probe` example measures endpoint setup, three requests
from one client and endpoint shutdown without printing snapshots or credentials.
It builds and passes Clippy. Production transport policy and timeouts are unchanged.


## Workspaces cards and action hierarchy

Workspaces now groups account capacity into one compact surface with all three
graphics retained at 800×600. Cards remove the decorative VM icon, reduce padding
and use a context-specific primary action: terminal, Start, Finish stopping or
Review details. The quieter Details action and clickable name still share one
workspace destination. T3 installation/readiness and another owner's identity are
visible without exposing provider sign-in forms or hardware diagnostics here.
Pending operations use neutral status and disabled progress buttons; GPU claims
remain distinct from a preference requested on the next start.

All 27 desktop tests pass, including selected-VM actions, Start preserving GPU
consent, pending-action suppression, review routing, T3 readiness and singular CPU
headroom. All 62 native iced screenshot baselines pass an ordinary comparison.
Normal, 800×600, wide/long-name, empty, over-quota, T3-ready, owner/GPU, cleanup,
updating and intervention layouts were opened for direct visual inspection.
Desktop Clippy, formatting and diff checks pass. The Mac app was rebuilt/reopened
and the actual Workspaces screen inspected with the live VMs. See
[UI evidence](../lab/evidence/workspaces-polish.json).

This uses existing snapshots and mutation APIs; no server deployment, VM lifecycle
operation or credential change was performed. Sidebar icon integration remains
separate. The guarded hotspot repeat is still explicitly deferred at the owner's
request; this UI pass makes no new off-LAN connectivity claim.


## Compact lifecycle controls and explicit force stop

Workspaces uses clickable VM names without a second Details action, filled green
or hollow gray status circles, shared amber transition states, and a fixed trailing
play/stop control. A leading official Material terminal icon supplements its text
label. Errors/unavailable states retain visible text. Hover and keyboard-focus
hints name the state/action; Tab/Enter activation is exercised in iced. Progress
starts before the RPC reply and survives an older snapshot. Reduced motion keeps a
clear static activity arc. Controller disconnection does not imply a stopped VM.

All 81 Rust workspace tests pass, including 30 desktop tests and 68 ordinary native
screenshot comparisons. The 50 local Python tests pass with two existing tests
skipped on the Mac's older Python; all 15 lifecycle fault tests also pass on the
Ubuntu host. Workspace-wide Clippy, formatting and diff checks pass. Direct visual
inspection covered normal/small cards, starting/stopping, hover tooltip, unavailable
status, recovery and the 800×600 force-stop confirmation. The rebuilt Mac app was
reopened and inspected against the live VMs.

The explicit force-stop mutation is separately audited, owner/admin authorized,
and serialized through SQLite's unique operation lane. Tests race graceful retry
against force, reject another user's request and stale completion, retain CPU/RAM/
GPU through failed shutdown or cleanup, and recover the durable intent after
reopening the database. Neither stop variant retries indefinitely after failure,
and no graceful timeout escalates automatically. Active/unrelated reviewed work
cannot be superseded. Only the adapter's verified shutdown plus complete GPU/CPU
cleanup permits claim release.

The Linux daemon, adapter and CLI were built and installed with rollback copies.
The controller-only restart preserved all managed VM states/processes, the T3
service process, and endpoint identity. No guest was stopped/restarted, no quota
was changed and no login was performed. A disposable running-VM force-stop proof
was not attempted: the configured 4 GiB RAM capacity is entirely allocated to the
owner's active devbox. That live proof, including real GPU reset after force, remains
pending. Fault-injection success is not recorded as physical force-stop acceptance.
See [evidence](../lab/evidence/workspace-controls.json).

The hotspot repeat remains deferred separately. Native VoiceOver integration is
an existing toolkit limitation; focus/tooltip tests do not establish OS screen
reader support.


## Unified workspace, confirmed stop and account metadata

Overview/Settings are now one workspace page with one resource summary/editor.
Advanced contains less-used hardware, technical, recovery and deletion controls.
Card header hit targets include empty space beside/below the name, and remain
separate from the stop control. Graceful Stop now requires confirmation. The
compact account dots describe a saved T3 link or provider sign-in; the aggregate
Ready to use chip is removed. Provider identity, allowance and actual successful
requests are deliberately distinct observations.

All 84 Rust tests pass, including 33 desktop tests and 73 ordinary native iced
screenshot comparisons. Tests cover confirmation/cancel/selected VM, header hit
geometry, unified resources, owner-only metadata, sign-in input preserved through
Advanced expansion, account-cache changes, unknown usage and read-only JSONL
provider queries. All 57 Python tests pass on Ubuntu; the Mac run skips two
existing tests requiring modern Python. Workspace Clippy, formatting and diff
checks pass. Direct visual inspection covered the enlarged header hover target,
account layout at normal/small sizes, the small stop confirmation, scrolled
Advanced controls, and the small Claude authorization form.

The updated daemon and fixed helper scripts were deployed with rollback copies.
All managed VM states/process IDs, the active T3 service process and host endpoint
identity were preserved. The live observation and controller database contain both
provider emails/plans and one Codex allowance window. T3 exposes account ID but no
email; Claude allowance is opened in its browser usage page. Only presence/counts
were saved in evidence; no account identifiers, codes, raw provider replies or
credentials were logged. No inference, login, guest restart, quota change or
application reinstallation was performed.

The rebuilt Mac app was reopened and its actual Workspaces screen inspected.
Native coordinate navigation was unavailable in the computer-use bridge
(`noWindowsAvailable`); page interactions and screenshots were verified through
the real iced view tree. This does not establish native VoiceOver support.
The previously deferred physical force-stop/GPU-reset and attributable hotspot
repeat remain pending. See [evidence](../lab/evidence/workspace-account-interface.json).


## Identity-only provider panel (supersedes allowance display above)

Following the UI review, Homecloud retains provider email, plan and sign-in state;
T3 handles ongoing usage management. Codex allowance bars, the Claude usage link,
usage parsing/model fields and background rate-limit requests are removed. T3's
missing CLI email is expected and now reads “Linked to T3”, without suggesting
incomplete configuration. The technical account ID remains under Advanced.

All 84 Rust tests pass, including 33 desktop tests and 73 ordinary screenshot
comparisons. All 55 helper tests pass on Linux; the Mac run skips the same two
modern-Python tests. Obsolete usage-parser tests were removed, and regression
coverage verifies account inspection ignores an old usage cache and invokes only
account/read. Account metadata remains allowlisted and owner-scoped. Workspace
Clippy, formatting and diff checks pass. Normal/small account screenshots were
opened for direct visual inspection, and the rebuilt Mac app was reopened.

The lab controller and helpers were updated with rollback copies. All VM states
and process IDs, the T3 service process and endpoint identity were preserved.
Live controller observations retain both provider identities/plans and a running,
linked T3 service, and contain no usage fields. Verification logged booleans only.
No guest restart, account change or inference request was performed. The deferred
hardware force-stop/GPU-reset and attributable hotspot proofs remain pending.
See [identity cleanup evidence](../lab/evidence/application-identity-cleanup.json).


## Compact healthy T3 connection row

The redundant “Linked to T3” caption is removed. The healthy row retains its
status dot/tooltip; setup instructions, sign-in progress and connectivity warnings
remain visible. The upstream status-JSON identity change and subsequent Homecloud
integration are recorded as unchecked TODOs in managed-apps.md. No upstream issue
or PR was submitted.

All 33 desktop tests and 73 ordinary native screenshot comparisons pass. The
existing compact-account test now also checks that connectivity warnings survive
caption removal. Normal and 800×600 account layouts were opened for direct visual
inspection. Desktop Clippy, formatting and diff checks pass. The Mac app was
rebuilt and reopened. This UI/documentation change required no server deployment
or VM/account operations.


## Apps & accounts grouping

The workspace card now uses the general Apps & accounts heading with T3 Code,
Codex and Claude rows. The divider between T3 and the providers is removed, and
row spacing is consistent. Install, authentication, progress and warning controls
retain their existing behavior. The upstream T3 status-identity TODO remains open.

All 33 desktop tests and 73 ordinary screenshot comparisons pass. Normal and
800×600 account layouts were inspected directly; all three account rows and
provider identity captions fit in the small screenshot. Desktop Clippy,
formatting and diff checks pass. The Mac app was rebuilt and reopened. No server
deployment, VM lifecycle operation or account change was needed.


## Unified Edit workspace and explicit restart impact

The workspace header now offers a Material pencil + Edit action for name,
configured RAM/cores/disk and GPU selection. GPU configuration moved out of
Advanced. The form updates No restart needed / VM stays stopped / Restart
required as fields change. Running resource edits show Save & restart… and a
second confirmation; Go back preserves the draft, and confirmation targets the
reviewed VM/draft. Allocation errors are visible near the heading. Renaming
changes display metadata only, preserving VM/SSH/network/application identities.

All 96 Rust workspace tests pass, including 46 store tests and 36 desktop tests.
All 80 ordinary native screenshot comparisons pass. The 58 Linux helper tests
pass; the Mac run skips the same two modern-Python cases. Workspace Clippy,
formatting and diff checks pass. Direct visual inspection covered normal/small
restart forms, the small confirmation, rename without restart, stopped edits,
and scrolled GPU choices. Existing comparisons also cover long-name headers and
provider setup. The rebuilt Mac app was reopened.

New tests cover server-enforced restart consent, atomic name/resource admission,
independent-connection CPU/RAM/disk/GPU races, GPU-claim migration, swap/detach
failure and retry after database reopen, stale confirmations, unchanged/invalid
forms, preserved input on Go back, and small-window GPU controls. Both old and
replacement GPU claims survive a failed transition. The host fault tests prove a
combined edit boots once, acknowledgement replay does not restart again, and
failed graceful shutdown applies no resource/device changes.

The Linux daemon, CLI and host adapter were built and installed with rollback
copies. The migration preserved all CPU/GPU claims and VM/application IDs. All
managed VM states/process IDs, the T3 service process and endpoint identity were
unchanged by deployment. Through the real authenticated API, a temporary rename
of the running devbox and a RAM edit of an already-stopped test VM both succeeded
without starting/restarting a guest. Original names/sizes were restored; resource
claims, SSH identities and application IDs were unchanged. Evidence stores only
booleans/hashes and test counts, with no account identifiers or credentials.

A real running combined resize/GPU-switch interruption was deliberately deferred:
the active devbox is in use and configured RAM is fully committed. The prior
physical force-stop/GPU-reset proof and attributable hotspot repeat also remain
pending. Fault-injection/replay results are not physical GPU-reset acceptance.
See [edit evidence](../lab/evidence/workspace-edit.json) and the edit procedure in
[operations](operations.md).


## Stable Edit dialog and multi-GPU selections

The Edit form now uses one Material checkbox per physical GPU, with multiple
selections supported and all unchecked meaning none. The changing restart heading
and description are removed; Save changes / Save & restart… plus the existing
confirmation convey the impact. Validation has a fixed helper area. Geometry
assertions cover rename, resize, invalid memory and GPU toggles without moving
form headings or actions. An unchanged selection preserves next-start preferences;
the rare boot-without-GPUs case retains an explicit forget-preferences action.

All 105 Rust workspace tests passed, including 50 store and 38 desktop tests.
Clippy passed for all targets with warnings denied. All 82 ordinary native
screenshot comparisons passed. The rename form, two-GPU selection at normal and
800×600 sizes, and small restart confirmation were opened for direct visual
inspection. These are native iced renders, not mockups. The rebuilt Mac app was
reopened and its connected Workspaces screen inspected.

Independent SQLite connections exercise overlapping multi-GPU starts and edits:
each request claims every selected GPU together with fungible backing or rolls
back completely. Old and new GPU claims survive failed edits and database reopen;
completion releases exactly the removed set. Tests cover normalized idempotency,
prospective permission changes, CPU-only consent, and legacy record upgrading.
Host fault tests cover preflighting every incoming card before shutdown, preserving
retained Incus device names, second-card preparation failure, all-card cleanup,
foreign/inherited/duplicate configuration rejection, all-device drift detection,
and replay without a second successful boot.

Linux core/store/daemon tests (66) and host-helper tests (66, no skips) passed.
The controller, CLI and privileged adapter were deployed after a guarded source
comparison. The upgrade preserved every guest PID/state, T3 MainPID, CPU/GPU lease,
VM/application ID and endpoint identity. Reversible live rename and stopped-VM RAM
edits were restored; the active guest and T3 were never restarted, and no GPU
configuration was changed by these live proofs. No active VM operations or VM
errors remained afterward.

The lab has one physical GPU. Simultaneous physical multi-card passthrough remains
pending on suitable hardware; mocks and transactional tests do not prove that
hardware acceptance. Existing NVIDIA guest-driver support is retained. Multi-GPU
readiness checks all selected cards respond, while per-device utilization is
explicitly unavailable until host/guest identities can be mapped safely. Running
combined resource/GPU interruption, real force-stop/reset and attributable hotspot
proofs remain pending. Evidence: `lab/evidence/multi-gpu-editor.json`.


## Remove the undocumented GPU RAM floor

GPU attach, start and resize/edit admission now use the ordinary 1 GiB VM minimum.
The old hardcoded 4 GiB GPU floor was removed from the store and desktop editor;
there was no documented upstream or hardware proof for that universal restriction.
The separate 4 GiB T3/Claude application recommendation remains unchanged.

All 51 store tests pass, including 1 GiB GPU attachment, legacy resize with an
attached GPU, multi-GPU edits and competing multi-GPU starts through independent
connections. Zero-RAM edits/resizes remain rejected; downsizing retains old backing
until host completion. All 38 desktop tests, 8 daemon unit tests and 3 transport
tests pass. All 84 ordinary screenshot comparisons pass, including two new 1 GiB
multi-GPU editor cases. The small native render was inspected directly. Clippy
passes for all targets with warnings denied. Linux store/daemon regressions (62)
also pass.

The controller was upgraded without changing guest/T3 processes, claims, VM/app
IDs or endpoint identity. A reversible live API edit saved a 1 GiB GPU preference
on an already-stopped VM, confirmed zero compute/GPU claims, then restored all
settings. Guest and T3 processes stayed unchanged. The Mac app was rebuilt and
reopened. This establishes admission behavior; booting a physical GPU guest below
4 GiB was not attempted on the occupied lab host. That hardware check remains
pending. Evidence: `lab/evidence/gpu-ram-policy.json`.


## Shared resource sliders and inline provider accounts

Create, Edit, Invite and resource-grant forms now share stepped native iced-m3
sliders with compact numeric fields. Memory and disk use whole GiB; CPU uses
physical-core steps. Disk edits cannot move below the existing size, and typed
shrink requests cannot be saved. Existing allocations above lowered quotas stay
visible and can be reduced. Grant bounds retain other users' promised or committed
backing, including disabled accounts. New forms initialize values inside available
ranges. Stopped CPU/RAM configurations are checked again on Start. Form ranges
stay fixed while open; server admission remains authoritative on Save.

Codex/Claude identity and plan appear beside each provider name in a subdued style;
long metadata wraps at narrow widths. Owner-only filtering, authentication and
provider usage behavior are unchanged. The only backend addition is read-only
`Snapshot.disk_overhead_gib_per_vm`, so storage controls use configured backing
overhead. Older snapshots default to the previous 2 GiB value.

All 115 Rust workspace tests passed. The 47 desktop tests include mouse dragging,
arrow-key adjustment, exact numeric typing, disabled disk shrinking, preserved
restart consent, owner-specific bounds, reduced quotas, zero grants, fixed ranges
during refresh and provider metadata wrapping. All 90 ordinary native screenshot
comparisons passed. Normal Edit/account layouts, small Create/grant forms,
over-quota and exhausted-capacity forms, and long email wrapping were opened for
direct visual inspection. These are native iced/tiny-skia renders. Clippy passes
for all targets with warnings denied.

Linux core/daemon/transport tests (16) passed. The metadata update was deployed
without changing guest PIDs/states, T3 MainPID, CPU/GPU claims, VM/application IDs or
endpoint identity. A read-only live API check confirmed reported disk overhead
matches controller configuration, no managed VM errors and no pending VM
operations. No VM resources were edited or guests restarted. The Mac app was
rebuilt and reopened. Evidence: `lab/evidence/resource-sliders.json`.


## Retain resource forms during dialog exit

Closing a resource dialog previously cleared slider bounds while iced-m3 retained
its surface for the exit animation. Rendering then substituted a short capacity
placeholder for all three controls, causing the panel to remeasure abruptly.
Resource ranges and outgoing errors now remain available for display through exit;
active input/submission lookup still requires an open panel. Duplicate Close does
not replace the outgoing panel. Opening a new form captures fresh bounds.

All 50 desktop tests and 90 ordinary native screenshot comparisons pass. Native
layout checks cover Edit, Create, Invite and Grant at normal and 800×600 sizes,
including validation errors, server errors and repeated dismissal. A real iced
runtime cache captures the open frame and exit at 0/32/64 ms. The initial exit frame
is pixel-identical to the open frame; the later frames were inspected directly and
retain the full form layout as Material's surface reveal/fade proceeds. The Mac
app was rebuilt and reopened. No backend or VM operation was involved. Evidence:
`lab/evidence/dialog-exit.json`.


## Contextual VM FABs

New VM and Edit VM now use iced-m3 extended FABs in the content pane's bottom-right
corner. The list heading is VMs. The workspace header keeps an outlined terminal
action and its power control. Final content has scroll clearance, and the FAB is
inset far enough to leave the scrollbar usable. Existing form/restart behavior is
unchanged.

All 54 desktop tests and 94 ordinary native screenshot comparisons pass. Four new
interaction tests cover fixed position at 800×600, 1180×820 and 2000×900, scrolling,
VM-specific routing, empty state, page scoping, mouse click-through, keyboard
activation, disabled/offline/pending state and modal blocking. The existing real
exit-frame comparison still passes. Normal, small, empty, scrolled, disabled and
modal renders were directly inspected; that inspection caught and corrected a
scrollbar overlap. Clippy passes for all targets with warnings denied. The Mac
bundle was rebuilt and reopened, and its live connected Workspaces page was
visually verified. No server deployment or VM operation was needed. Evidence:
`lab/evidence/vm-fabs.json`.


## Equal FAB edges and VM terminology

The page content's right edge and both FABs now align 24px from the window edge;
FAB bottom spacing remains 24px. The former 36/48px right inset is removed. A
separate 16px outer scrollbar gutter keeps the track clear without increasing
content/FAB padding. Headers and inline banners share the content alignment.
The existing wide-window content width cap remains in place.

Visible navigation, page/dialog titles, field labels, lifecycle hints, activity
labels and status messages now use VM/VMs consistently. Internal API names and
screenshot filenames are unchanged. Delete-button tests use its VM-specific ID
because its heading and button now share the same visible label.

All 54 desktop tests and 94 ordinary native screenshot comparisons pass. The
placement test checks equal 24px FAB insets and scrollbar separation. Normal VM
cards, small scrolled detail actions, small Hosts and Edit VM renders were opened
for direct inspection. Formatting, whitespace checks and Clippy with warnings
denied pass. The Mac bundle was rebuilt and reopened; the live connected VM page
was visually verified. Evidence: `lab/evidence/vm-fabs.json`.


## Hosts directory, flat allocations and reserve visibility

Hosts now opens a machine directory and each host opens its own detail view with
pinned Overview, Allocations and Hardware tabs. The current authenticated
controller still supplies one host; fleet discovery/aggregation is deferred.
Allocations shows memory, CPU and storage together per person with expandable VM
rows, retaining VM-specific grant editing and member-only scoping. Resource-tab
state and its message were removed.

The admin Overview adds read-only Host headroom. Memory/cores outside the VM budget
include host services and unmanaged workloads; they are not described as exclusive
reservations. The pool operating floor is reported from root-owned host policy as
optional `HostInventory.minimum_pool_available_bytes`; older inventory reports
Not reported. No value is inferred from ZFS's changing free space. Safe admin-only
reserve editing is tracked in `docs/mvp.md#admin-host-reserve-editing`.

All 127 Rust workspace tests pass, including 58 desktop tests and the privileged
adapter's Python suite. All 101 ordinary native screenshot comparisons pass.
New native interaction tests cover directory/detail navigation, the single resource
view, per-owner expansion and grant routing, member privacy and missing disk-policy
metadata. New core coverage checks compatibility with older inventory. Normal and
800×600 directory/allocation views, expanded VMs, headroom and missing-policy
renders were opened for direct inspection. Workspace Clippy with warnings denied,
formatting and whitespace checks pass.

Fourteen Linux core/daemon tests passed before the metadata-only lab upgrade. The
upgrade verified unchanged host policy, CPU/GPU claims, VM/application IDs, guest
PIDs/states, T3 MainPID and endpoint identity. The live authenticated API reports the
configured pool floor and matching unchanged capacity with no VM errors or pending
VM operations. The Mac bundle was rebuilt/reopened, and its live Hosts directory
and selected host Overview/headroom were visually inspected using native keyboard
navigation. Evidence: `lab/evidence/hosts-directory.json`.


## Admin host reserve editing and recovery

Admin Overview now edits additional memory/core/disk reserves within the installed
reviewed VM envelope. SQLite admission protects each user's larger grant or held
commitment in every dimension. The root adapter independently verifies promises,
complete unclaimed SMT groups, current host CPU claims and actual pool headroom.
A private fsynced journal and boot replay coordinate live/persistent CPU fences;
capacity releases wait for confirmed host enforcement. Unknown policy fails closed.

All 145 Rust workspace tests and 108 ordinary native screenshot comparisons passed.
The final stale-refresh adjustment passed its five targeted GUI tests. All 77 Python
tests passed with two existing skips, including 11 reserve-adapter fault tests.
Ubuntu passed 82 Rust tests and the final 11 reserve-adapter tests. Workspace Clippy
with warnings denied, formatting and whitespace checks passed. New tests cover
concurrent grants/creates versus reserves, stopped/pending commitments, disabled
users, idempotency/revisions, partial host effects, lost acknowledgements, durable
discard tombstones, policy mismatch, restart recovery, member privacy and stable
native dialogs. The editor, small/invalid/no-headroom layouts and progress/recovery
screens were opened for direct inspection.

The matched Linux controller/adapter upgrade retained every guest PID/state, T3
MainPID, CPU/GPU lease, VM/application ID, endpoint identity and baseline host config.
Live attempts to reserve already-promised RAM, CPU and disk were rejected. After
explicitly lowering only unused owner grants, an extra whole physical core [5,11]
and 32 GiB storage reserve applied. Host/user CPU fences expanded to housekeeping
[0,1,5,6,7,11], VM capacity became 4 GiB/1 core/96 GiB, and the configured pool floor
rose from 30 to 62 GiB. Running guest vCPU affinities and T3 stayed unchanged.
Attempts to issue the reserved CPU or storage capacity again were rejected.

During release, a lab-only lock held the privileged helper while the controller
persisted Applying. The controller was paused; the helper completed host effects,
then the controller was killed before its database completion. On restart it used
the matching applied host proof to converge, retaining conservative capacity until
confirmation. Replaying the production boot CPU-layout entrypoint also preserved
all guests. Original grants, 4 GiB/2-core/128-GiB capacity, zero additional reserves,
30-GiB floor and original CPU fences were restored. No full host reboot was performed.
Positive live RAM changes were not attempted because all 4 GiB is held by the devbox;
automated tests cover them without disrupting that workload.

The first live attempt safely exposed an overly conservative adapter check that
counted fully deleted VM history as current CPU ownership. It failed before any
CPU effects. The operation was durably discarded, the check was corrected, and a
regression verifies that deleted history is ignored while unconfirmed stopped
claims remain protected. The full live sequence then passed.
Evidence: `lab/evidence/host-reserves.json`.


## Total host reserves and elastic ZFS ARC policy

The admin editor now uses total system memory/physical-core reserves and total pool
free-space floor. Explicit root-reviewed minimums define the maximum VM envelope;
the internal durable record keeps the amount above those minimums. Lowering the old
conservative host allowance releases unissued capacity without raising user grants.
Increasing it still protects each user's larger grant or held commitment. ARC's
maximum is independently configurable as native Automatic or a custom limit and
does not consume guest capacity. The host reports actual ARC size, minimum,
effective maximum and configured mode. Cache occupancy above a newly lowered cap
is shown honestly and does not block completion.

The privileged adapter journals intent before effects, rejects foreign ARC config,
persists the managed modprobe setting, rebuilds initramfs and verifies effective
`c_max`. Automatic uses the reviewed native ceiling live and persists zero for
module load. CPU/RAM-only changes skip rebuilding an already-verified boot cache
policy. An interrupted persistence attempt retains its rebuild requirement, including
when a host reboot interrupts the original initramfs update.
Unreviewed automatic defaults cannot be enabled. An already-managed Automatic
policy on an unreviewed ZFS upgrade fails closed and needs local root policy review
before admission resumes.

All 150 Rust workspace tests passed, including 65 GUI tests and 113 ordinary native
screenshot comparisons. The final GUI recovery adjustment passed all 65 desktop
tests again. The Python suite ran 88 tests with two existing skips. Linux passed
85 Rust tests and 14 reserve-adapter tests. Clippy with warnings denied, formatting
and whitespace checks passed. New fault coverage verifies ignored kernel tuning,
initramfs failure/retry, native-zero boot semantics, cache occupancy above its cap,
independent capacity accounting and controller restart after a lost host acknowledgement.
Normal/small/scrolled editors, Automatic mode, cache status and the live Mac app
were opened for direct visual inspection.

The guarded local-root migration retired only two named stopped legacy proof VMs,
preserving the managed devbox, T3 process, all resource claims and endpoint identity.
The 16-GB lab profile has a 4-GiB operating memory minimum and one complete physical
core minimum. Lowering total reserves to those minimums exposed 11 GiB/five cores
for VMs; original owner grants remained unchanged. A custom 1-GiB cap and native
Automatic each applied without changing capacity or grants.

Four disposable VMs and the devbox claimed the full 11-GiB/five-core guest budget.
The pressure test grew guest memory while bounded storage writes and a scrub ran.
The first 55-second run reclaimed ARC and kept every guest job healthy, but exceeded
a conservative 64-MiB paging guard; it was not marked passed. Original grants and
the custom 2-GiB cap were restored. The follow-up tests lasted 120 seconds and required
total paging below 1% of physical RAM, no more than 16 MiB during its final 30 seconds,
final memory PSI avg10 below 1%, healthy guest jobs/pool, no host OOM and the selected
cache policy's effective cap. Exact allocations, ARC samples and paging are in the evidence.
This is a measured lab profile, not an OOM-free guarantee for arbitrary workloads.

Initial disposable-guest setup exposed a transient APT DNS failure and an unavailable
Incus guest agent. Only those two unpaired proof guests were restarted/repaired;
the permanent devbox stayed running. New guests now configure three APT download
retries. A later first-boot agent failure was traced to a mismatch between the
configured Incus VSOCK ID and running QEMU ID; an explicit restart of only that
unpaired proof guest repaired it. Its underlying cause remains a provisioning
reliability follow-up, not an automatically repaired condition. All disposable
VMs, load units and the tagged cache dataset were removed.
Original 4-GiB/two-core user grants were restored. Final system reserves are 4 GiB,
two physical cores and a 30-GiB pool floor; the VM budget is 11 GiB/four cores/128 GiB
storage. The longer native Automatic experiment still paged several GiB, despite healthy
guest jobs and no OOM events. It was not promoted. The custom 2-GiB cap then passed
the same bounded guest-memory/write/scrub and settling checks. Native Automatic
remains an explicit option, and the GUI explains the possible paging cost.

A full physical-host reboot then verified the selected custom 2-GiB parameter
at module load and its effective maximum, persisted reserves and CPU/GPU claims,
endpoint/VM/app identities, automatic devbox/T3 recovery and both providers' existing signed-in state.
No new paid provider task was submitted. The Mac bundle was rebuilt/reopened and
the final live Overview/editor inspected. Off-host backup remains explicitly deferred.
Evidence: `lab/evidence/host-memory-policy.json`.


## Reliability follow-ups and system-backed ARC

This supersedes the native-Automatic recommendation in the previous section.
Homecloud Automatic now uses a reviewed positive cap inside the total system
reserve. The lab profile splits its 4 GiB system minimum into a 2 GiB operating
floor and 2 GiB ARC allowance. Raising the cache ceiling requires backing inside
that allowance; a larger allowance requires more unpromised system reserve.
Neither occupancy nor RSS changes grants.

Investigation reproduced a QEMU orphan after a failed Incus launch and later
VSOCK mismatches around first-boot reboots. The controller had been providing
cloud-init both through the image's agent templates and an extra NoCloud disk.
The exact Incus 6.0.5 agent source removes `/run/cloud-init` and requests a reboot
after applying its seed templates. Guest journals captured cloud-init running
from the extra disk during that removal, failing to write its network-state
symlink. On the next boot one VM considered the network stage already handled,
leaving its NIC unmanaged; its package installs then failed DNS resolution.
New VMs use one reviewed seed transport. Existing devboxes are left intact.

The root adapter now holds a durable launch marker through first-boot reboot and
checks actual QEMU processes and active Incus operations before another start.
This closes a controller retry race with Incus's internal restart. Agent/setup
waits have durable bounded deadlines; VSOCK conflicts require review and retain
claims. There is no automatic restart or force kill. An unfinished Create can be
superseded by confirmed Delete without prematurely freeing CPU/RAM/disk claims;
a root deletion fence rejects a delayed stale Create. The failed package fixture
was deleted normally through this API. The earlier orphan required explicit,
identity-checked lab cleanup; its claim was held until the process exited.
The precise cause of the original failed launch/orphan remains unproven: 24
small standalone launches, half stock QEMU and half through the CPU launcher,
passed. Pin and retest the Incus/QEMU/AppArmor integration on upgrades.

With the single-seed transport, seven new VMs completed cloud-init with matching
VSOCK IDs and no manual guest repairs. These included a 4 GiB recovery VM and the
four later memory-load fixtures. APT uses three acquisition retries and forces
IPv4 to match the V1 guest network. Controlled SERVFAIL injection in the disposable
VM recovered after two failed DNS replies and downloaded a real package. Persistent
SERVFAIL failed with exit 100 after bounded retries; the original resolver was
restored in both cases. The injection initially exposed an unusable IPv6 fallback
after an IPv4 DNS failure; the IPv4 acquisition policy addresses that corner.
Persistent package/setup errors still require explicit inspection, repair or
normal deletion; retry does not wipe a disk or erase cloud-init history.

The physical-host ARC proof rejected a 3 GiB ceiling inside the 2 GiB allowance.
A 6 GiB total reserve backed a 4 GiB cap and a cache warmed above 4 GiB. Releasing
2 GiB of reserve kept VM capacity at 9 GiB until actual ARC occupancy fell below
2 GiB; only then did capacity return to 11 GiB. The temporary free-memory target
was restored. Selecting Homecloud Automatic then enforced a numeric 2 GiB cap.

Real persistence faults ran inside a marked Ubuntu 24.04 disposable VM with its
own OpenZFS kernel module and actual `update-initramfs`. The helper was killed
before generation, during the real process-group build, and after successful
generation before acknowledgement. Each actual VM reboot replayed the production
ARC/journal recovery, restored the temporary reclamation target, confirmed the
256 MiB fixture cap and marked the journal applied only after persistence finished.
CPU placement was excluded from this component fixture; physical-host CPU fences
have separate live proofs. The physical server and permanent devbox were not
rebooted by these tests.

The versioned lab runners store fsynced manifests, owner IDs and idempotency keys,
observe pending/retrying phases, serialize local runs, retain failed fixtures and
resume cleanup through the normal API. Root load helpers accept only recorded,
marked disposable VMs without managed apps. Full guest admission uses all 11 GiB
and five physical cores, while the new two-minute Automatic/custom memory samples
confine allocation load to the four disposable guests. A first comparison was
rejected because available-memory-based working sets differed; both final runs
use identical fixed resident working sets. The existing 4 GiB devbox
stays idle. This is not a fresh test with every configured byte resident at once,
nor a universal zero-swap guarantee. Final measurements and cleanup are in
`lab/evidence/reliability-followups.json`.

All 152 Rust workspace tests and 113 native screenshot comparisons passed, along
with 99 host Python tests (two existing skips), seven lab-harness tests, 81 Linux
Rust tests, formatting and warnings-denied Clippy. A timing-dependent Claude test
fixture was corrected to wait for published readiness before submitting its fake
code; no provider credentials or actual model requests were used. The reserve
editor, Automatic, small/scrolled layouts, cache summary and reopened live Mac app
were directly inspected. The matched backend upgrade preserved permanent VM/T3
processes, resource leases and endpoint identity. Deferred hotspot tests and
off-host backups remain separate, unchanged work.

Upstream references: [Incus cloud-init seed alternatives](https://linuxcontainers.org/incus/docs/main/cloud-init/),
[Incus 6.0.5 agent seeding/reboot](https://github.com/lxc/incus/blob/v6.0.5/cmd/incus-agent/main_agent.go),
[OpenZFS ARC free-memory target](https://openzfs.github.io/openzfs-docs/man/master/4/zfs.4.html#zfs_arc_sys_free).


## Extended startup, diagnostics and isolated failure validation

The full API soak completed 51 fresh VMs, 17 stop/start resource-release and
readmission checks, and six immediate Create cancellations. All fixtures were
removed normally and the captured grant restored. There were no VSOCK mismatches
or non-pending startup errors. A bounded observer recorded 768 process/syscall
metadata events, including 46 one-byte reads and 13 writes returning one; it did
not record buffers and was stopped afterward. This did not reproduce or establish
the cause of the earlier QEMU orphan. The launcher and AppArmor were unchanged.
The durable launch/process guards remain the protection for that unresolved case.

Setup stages now survive operation retries, and unfinished operations cannot age
out behind the recent completed-history limit. VM details expose owner/admin-only,
on-demand cloud-init log tails, explicit administrator retry after repair and
confirmed deletion of an incomplete Create. Retry checks existing setup rather
than rerunning failed cloud-init modules. Log data is not stored in the controller.
Normal/small native screenshots and the running Mac app were inspected. A further
fresh VM verified that the details action opens during provisioning and returns
real guest logs; it was then deleted and its grant restored.

Live inspection caught an escaped-newline bug in the initial guest log reader;
the executable regression test now verifies the actual program, bounded output,
and nonblocking refusal of a FIFO. Pending explanatory text no longer moves the
button between retries. Review also corrected the fresh installer's reference to
the ARC module. These fixes are implemented, not pending recommendations.

Five real persistence faults ran in a marked 7 GiB/32 GiB Ubuntu test VM: read-only
configuration, a full loop-mounted ext4 /boot, and virtual hard power-off before,
during and after initramfs generation. Failed persistence stayed Applying. After
repair/reboot, each recovered the fixture's 256 MiB cap, restored its temporary
ARC free-memory target and acknowledged only completed persistence. CPU placement
was excluded from this component test. The physical host and working devbox were
not rebooted or subjected to these faults.

The first nested memory fixture used a regular-file ZFS vdev. It failed the paging
guard: 645,726,208 bytes paged out by the last 533-second sample, with about 2.6 GB
of Linux file cache beyond shmem as well as ARC. This run was rejected. The pinned
OpenZFS file-vdev path uses ordinary file I/O; a loop block device with verified
direct I/O removes that extra backing-file page cache from the fixture.
See [OpenZFS file-vdev source](https://github.com/openzfs/zfs/blob/zfs-2.2.2/module/os/linux/zfs/vdev_file.c)
and [losetup direct-I/O options](https://github.com/util-linux/util-linux/blob/master/sys-utils/losetup.8.adoc).

The corrected nested worker preallocated its full 2 GiB guest budget across two
VMs, continuously touched guest pages and performed bounded ZFS reads/writes.
Guest RAM mapping residency was checked initially and periodically, excluding
QEMU's own overhead. Both 600-second Automatic/Custom windows at the same 2 GiB
cap passed with zero paging, zero new OOM kills, final memory PSI avg10 zero,
healthy workloads/pool and every checked guest mapping resident at 1 GiB. These
are policy-path and sustained-health checks, not a performance comparison between
identical caps. A final cap reduction reclaimed ARC below the 1 GiB allowance.
A cleanup-only variable-shadowing bug was fixed; removal of the remaining known
swapfile was explicitly completed and verified. Nested VMs, pool, loop device and
swapfile were removed, then the outer worker was deleted through Homecloud.

This is a full resident budget in a disposable nested worker, not a new full
physical-host saturation/max-density proof. Its Ubuntu 24.04 Incus/QEMU/ZFS versions
are recorded separately from the Ubuntu 26.04 physical lab. Virtual hard power-off
also does not establish durability under actual physical power/write-cache loss.
The original orphan cause, eventual-hardware density tests, hotspot repeats and
off-host backups remain explicit limits or deferred work.

All 158 Rust workspace tests, 68 desktop tests (included in that total), 118 native
screenshot cases, 103 Python adapter tests (two Mac-only skips; none on Linux),
seven lab-harness tests, 90 Linux Rust tests, formatting and warnings-denied Clippy
passed. The matched backend was deployed and the final Mac app rebuilt/reopened.
Existing VM/T3 processes, claims, endpoint identity and reserve policy were preserved.
Reviewed evidence: `lab/evidence/reliability-validation.json`.


## Targeted QEMU launch investigation and reboot race

The exact Ubuntu Incus/QEMU source archives and packaging patches were reviewed.
A targeted guest reboot plus concurrent Start reproduced two QEMU generations: the
first parent acknowledged readiness successfully; the second child failed to lock
the existing PID file, its parent read EOF and exited 1. The surviving process
belonged to the first successful launch. Incus's stored VSOCK CID and generated
config agreed with each other but differed from the actual guest kernel CID, read
over enrolled, host-key-checked SSH. The agent timed out, claims stayed held and
the operation required review. Only that unpaired proof VM received explicit
graceful repair; all three fixtures were then deleted normally and grants restored.
This did not reproduce the earlier deleted-instance orphan itself.

Repeated Running intent now records a transactional, durable no-op rather than
enqueuing a second physical start. Both API forms retain existing errors, GPU
preferences and all claims. Concurrent database-connection regressions cover the
transient stopped observation, request replay and the prior CPU-only consent path.
The Linux-tested daemon was deployed with a database backup, preserving permanent
VM/T3 processes, leases, endpoint identity, reserves and the existing CPU launcher.

The post-fix campaign completed 24 full fresh provisions, 8 stop/start checks,
8 guest-reboot/concurrent-Start checks with actual guest CID and agent verification,
8 cancellations during active creation after QEMU launched, and 4 immediate
cancellations. All 36 durable Create intents were cleaned through the normal API and
the original grant restored. Trace attribution counted 76 VM launch generations,
with 76 acknowledged parent readiness checks. Full private traces distinguish
parent failures and earlier surviving processes; child write-side coverage is not
claimed complete. No QEMU/AppArmor changes or Incus/QEMU upgrades were made.

The new userspace witness initially retained SQLite handles and reached its soft
descriptor limit. Explicit connection closing and a 500-sample real-database
regression fixed that tool-only defect. Its replacement caused a documented
userspace-witness gap; the kernel trace continued. Bounded API samples can also
fail during shutdown/deletion, and those skipped samples remain in evidence.
The working controller did not exhibit this handle leak.

All 160 Rust workspace tests, 118 native screenshot comparisons, 86 relevant Linux
Rust tests, 11 focused lab-tool tests, formatting and warnings-denied Clippy passed.
Permanent VM/T3 processes and original grants were preserved; disposable VMs and
observer units are stopped/removed, the pool is healthy and no new kernel OOM
events were found. See [investigation notes](qemu-startup.md) and
[reviewed evidence](../lab/evidence/launch-investigation.json). The original orphan
cause, final-hardware density, hotspot repeats and off-host backups remain separate
limits or deferred work.


## Repeated-action audit

The mutation, operation-ownership and retry paths were reviewed after the targeted
reboot investigation. Three additional gaps were addressed:

- An unchanged legacy Resize could enqueue an Apply that reached Incus Start.
- A redundant legacy GPU detach could do the same; stopped-VM GPU preference
  changes also did unnecessary physical work.
- Successful host setup cleared its durable launch guard before the controller
  committed the acknowledgement. A lost response could therefore replay without
  that guard during an internal guest-reboot gap.

The first two now complete locally without changing observed state, errors or
claims. The host keeps its launch guard through the running allocation's lifetime;
confirmed shutdown clears it, and verified deletion removes leftover state.
Before the fixes, focused regressions failed on queued host work and the missing
launch marker. Afterward, independent database connections submitted 28 conflicting
requests across seven mutation types against one unfinished resize: all were
rejected without changing claims or replacing its intent. A daemon regression
reconciled 24 fresh-key no-op requests without dispatching a host Apply; a real
resize was its positive control. The host regression models successful apply,
lost acknowledgement, the empty-process reboot window, and explicit stop/start.

All 164 Rust workspace tests (including 68 desktop tests and 118 native screenshot
comparisons), 90 relevant Linux Rust tests, formatting and warnings-denied Clippy
passed. All 104 Python host tests passed on Linux; the Mac run had two existing
Linux-only skips. No UI or screenshot baseline changes were made in this pass.

On the physical lab, 62 command attempts against one disposable VM comprised
56 local completions and six unchanged-Edit rejections before RPC. Three guest
reboots with concurrent Start/Resize/GPU-detach bursts each produced exactly one
new QEMU launch. The actual guest CID matched Incus and guest-agent access recovered.
Stopped-VM GPU preference set/forget changed neither physical devices nor claims.

A separate disposable VM retained its marker after completed setup and through a
real guest reboot with a host Apply replay. Its launch count increased by one,
its actual guest CID remained consistent, and a settled replay added no launch.
Normal Stop cleared the marker; Start created one new launch and a new marker;
Delete removed it. This exercises replay at the host boundary, not an actual
controller-crash injection; the deterministic unit test covers the precise empty
process window. The first replay result was not saved before a harness assertion,
so its exact pending stage is not claimed. Initial assertions wrongly required
immediate completed setup after agent recovery. The failed manifest/log were kept;
resuming verified the unchanged launch count and subsequently completed replay.
The first burst harness also incorrectly treated the safe unchanged-Edit CLI
rejection as a failure; its original log and manifest were preserved.

The daemon was deployed with a database and binary backup. The helper pair was
installed under the host-operation lock with backups and no VM/Incus restart.
Final verification found only the two permanent VMs, zero unfinished VM operations,
no untracked Homecloud QEMU, no GPU leases, original grants restored and pool ONLINE.
The permanent QEMU and T3 service PIDs, endpoint identity and reserve policy were
preserved. Installed binaries/helpers match tested sources/builds. See
[reviewed evidence](../lab/evidence/action-audit.json).

This is not an exhaustive proof of every Incus-internal transition, crash point or
cross-resource interleaving. Distinct legitimate changes may still execute
sequentially; there is no global cooldown. The audit also found that desktop manual retries after an ambiguous transport
failure generated fresh idempotency keys, allowing a second distinct VM if quota
permitted. This client recovery gap is addressed in [Durable desktop Create
recovery](#durable-desktop-create-recovery) below; physical admission remained
transactional throughout. The earlier
deleted-instance orphan remains unproven, and final-hardware density, hotspot repeats
and off-host backups retain their previously documented status.


## Durable desktop Create recovery

The client retry-key concern identified in the repeated-action audit is addressed.
The desktop now persists the immutable Create payload, request key and public
host/device binding before sending. It restores and replays that record after app
restart; retry bypasses new-allocation GUI checks. Its recovery dialog provides
Close and Retry request, freezes the submitted configuration, and preserves its
body during dismissal. Late responses cannot clear another submission or overwrite
another VM's open editor. Another desktop instance cannot overwrite the journal.

One addition beyond the initial client-only plan was necessary: the server also
commits rejection receipts. An authenticated `CreateRejected` outcome fences late
copies of the old request key, allowing the user to amend the draft safely.
Generic authentication/server/transport errors remain unresolved. Partial admission
failure rolls back allocations before the negative receipt commits. Accepted
requests continue returning their original Operation before quota admission.

Regressions cover response loss followed by both client/database reopen, replay
with quota entirely consumed, eight concurrent independent database connections,
rejected-key replay after quota increases, legacy replay of rejected keys, changed
payloads, a failure injected after partial application/VM/lease creation, corrupt or
mismatched journals, competing windows, late replies, and intentional new creation.
A real QUIC test sends Create, waits for its database commit, closes without reading
the response, reconnects, and confirms one VM/operation/allocation. It also checks
committed rejection replay and that revoked-device errors remain generic errors.
These tests use temporary state and simulated host execution; no new physical VM
or provider task was needed for this pass.

All 176 Rust workspace tests passed, including 75 desktop tests. The final desktop
state fixes passed the complete desktop suite and focused navigation regression.
All 101 relevant Linux Rust tests, formatting, and warnings-denied Clippy passed.
Three new native iced screenshot baselines cover recovery at normal/compact sizes
and its checking state; all three were directly visually inspected. Together with
the existing suite this makes 121 screenshot comparisons.

The matching controller was deployed with a database/binary backup and the Mac app
rebuilt. Final host verification preserved the permanent devbox QEMU and T3 service
PIDs, original resource grants and claims, endpoint identity and reserve policy.
Only the two permanent VMs remain, no VM operations are unfinished, no untracked
Homecloud QEMU exists and the pool is ONLINE. There was no Incus/QEMU/helper change
or workload restart. Evidence: `lab/evidence/create-recovery.json`.


## Physical RAM upgrade: 48 GiB installed

After the clean power-off and DIMM installation, firmware reported 16, 8, 16 and
8 GiB DDR4 devices, all configured at 2133 MT/s. Linux reported 48,217,760 KiB
MemTotal (45.984 GiB); the boot log includes a 1 GiB crash-kernel reservation.
This is a capacity/boot check, not a full memory burn-in test.

The host-layout unit failed before Incus/controller startup with `ARC minimum
exceeds the reviewed operating allowance`. Effective ARC c_min was 1,542,968,320
bytes versus the old 1 GiB reviewed threshold; c_max remained 2 GiB. After backing
up the database and configs and taking both writer locks, the reviewed threshold
was raised to 2 GiB and the RAM envelope expanded from 11 to 41 GiB consistently
in host.json, control.json and meta.limits. The total system minimum remains
4 GiB (2 GiB operating floor + 2 GiB cache allowance). The migration preserved all
user/device/VM/application/operation records and CPU/GPU/network leases exactly,
and added a local-root audit event. Endpoint identity, reserve journal and ARC
boot configuration hashes remained unchanged.

The same host-layout boot path then passed. Incus and Homecloud started, the
existing devbox auto-started, its agent responded, T3's user service was active
and its loopback listener was present on port 3773. There are no failed systemd
units and the pool is ONLINE. An authenticated Homecloud snapshot reports the
41 GiB VM budget, original 4 GiB user grant/commitment, both expected VM states,
no VM errors and no unfinished VM operations. No source/binary/kernel/ZFS changes
were needed. See `lab/evidence/ram-upgrade.json` and the current
[operation notes](operations.md#physical-ram-upgrades).


## Managed full-host memory campaign — October 29

A disposable administrator-managed Homecloud host ran its own Ubuntu 26.04,
OpenZFS 2.4.1, Incus 6.0.5, QEMU 10.2.1 and Rust controller, with a real inner VM.
The outer controller owned its compute and independently backed 32 GiB test disk.
Evidence: `lab/evidence/managed-host-memory.json`.

The 8 → 36 → 8 GiB campaign passed. Growth retained the original 3 GiB inner VM
envelope until admin adoption, while resuming the already-held VM once. Adoption
expanded the reviewed envelope to 31 GiB without altering grants. A live 1 GiB
ARC cap coordinated the effective minimum from 1,179,937,024 to 536,870,912 bytes,
then verified `c_max=1,073,741,824`; numeric min/max survived the subsequent boot.
The ARC boundary model reproduces those recorded values from physical memory and
is tested through the real Python adapter code.

With 6 GiB held after shrinking to an 8 GiB worker (3 GiB safe VM envelope), the
inner VM remained off and retained its claim. An unsafe running edit was rejected
before intent. Explicit Stop released compute only after verified cleanup; a
stopped resize and grant correction permitted restart with the same identity and
proof file. An attempted start before correcting the promise deficit was rejected.

The campaign also exposed and fixed fresh-install parent permissions under a
restrictive umask, missing seed-ISO prerequisites, unset ARC wire normalization,
and read-only probe races during shutdown. The original Incus start error now
survives the bounded launch-wait record and is available in VM diagnostics.

One shrink boot had an unexplained Incus SIGKILL. Kernel logs showed no OOM event,
and the inner VM had `boot.autostart=false`. The controller initially waited behind
Incus despite its weakened dependency; that remaining ordering was removed and
read-only Incus calls bounded. While Incus remained unhealthy, management returned
in 8.86 seconds with the 6 GiB claim and 3 GiB capacity intact. An explicit Incus
restart recovered the daemon and the inner VM stayed off until safe recovery.
The SIGKILL cause is unresolved; this evidence does not attribute it to ARC or RAM.

The original worker was deleted through the normal API, including its owned test
volume, and its outer grant restored. A separate fresh-install/reboot proof checks
the final bundle without manual guest repair. Nested tests do not establish
physical-host memory density, GPU/IOMMU behavior, or hardware power-loss durability.


The clean follow-up passed without manual guest installation repairs; see
`lab/evidence/managed-host-fresh.json`. It bootstrapped an independent test admin,
created its first inner VM with the native image agent transport, excluded the
worker uplink from bridge allocation, and completed the first ARC change.
Controlled Incus/socket failure left management reachable and VM observations
unavailable with disk claims retained. An injected 3 GiB effective ARC maximum
against a 1 GiB policy blocked starts before claiming compute; the outer managed
application reported memory-policy attention, and inspection left the fault
unchanged until explicit repair.

The service-fault fixture initially restored Incus's socket and daemon but missed
`incus-startup.service`, which stopping the socket had also deactivated. This
removed the normal guest shutdown hook and an immediately preceding unflushed
proof write was lost. The fixture now restores all three units. Its repeated cold
boot preserved both an explicitly fsynced file and an ordinary buffered write,
resumed the existing VM, and retained the numeric ARC cap. The earlier unexplained
Incus SIGKILL did not recur on the fresh worker's boots. No claim is made about
unsynced data surviving an actual abrupt power loss.


Final local verification passed 190 Rust workspace tests, 119 Python tests
(117 passed, two platform skips), and 126 native iced screenshot comparisons.
The managed-host creation/details and RAM review/deficit screenshots were directly
inspected. Formatting, diff and Mac bundle signature checks passed. The final
Mac app was reopened and visually checked against the restored original grant.

Both disposable workers and their owned test disks were deleted, with no remaining
CPU/GPU/network leases. The physical host retained its 41 GiB reviewed VM envelope,
original user grant (4 GiB, two cores, 128 GiB disk), commitments (4 GiB, one core,
48 GiB backing), and two permanent VMs. The devbox QEMU PID remained 3361 and its
T3 service stayed active; no permanent workload restart was needed. The pool is
ONLINE, the authenticated snapshot has no warnings or unfinished VM operations,
and final deployment hashes are recorded in `lab/evidence/managed-host-final.json`.


## Memory recovery UI and two-user campaign — October 29

The GUI now keeps a typed host-memory warning above every page until verified
recovery. It distinguishes excess promises, excess held memory, ARC drift and an
unverified policy. Admin Review host links to affected VMs, grants, reserve/cache
editing and RAM-capacity review. Blocked known-stopped VMs have a stationary
Startup blocked status and a confirmed Stop/release path. Unknown power remains
unknown in both the header and apps panel. Members receive the condition and
their own VMs, without other owners or aggregate allocation details.

The real nested worker ran two ordinary users' VMs through 12 → 10 → 8 GiB. Both
resumed with 6 GiB promised and 4 GiB held against a 5 GiB VM budget. Both stayed
off with the same claims against a 3 GiB budget. Stopping Alice's VM released its
compute only after cleanup and let Bob's held allocation resume. New memory claims
remained blocked until grants were corrected. Both fsynced proof files survived.

Four concurrent Start requests raced two grant increases. Both VMs ended with
one QEMU process and distinct CPU cores; exactly one grant increase acquired the
last GiB. Final inner health was Healthy: 3 GiB promised, 2 GiB held, 3 GiB budget.

Real ARC fault injection exposed two repair gaps. First, `applying` conflated an
uncompleted reserve job with ARC drift, blocking explicit repair. It now reports
the durable journal phase, while admission, workload reconciliation and reserve
completion independently require ARC enforcement. Second, an initial host-configured
ARC mode could make a mode-less reapply a no-op. The controller refused the unsafe
completion and retained its fence. The synthetic fault was rolled back to finish
that receipt; the backend now rejects mode-less drift repair before effects and
the GUI proposes a concrete backed limit. The resumed real proof applied an
explicit 2 GiB limit and verified recovery without starting guests into the RAM
shortage. These are covered by positive and negative regression tests.

Visual inspection also found the reserve editor advertising its old reviewed
envelope after a physical shrink. It now uses the smaller physical envelope,
offers no extra memory headroom during a promise deficit, and remains usable for
cache repair. Seven new native screens were directly inspected, including compact
admin/member warnings, blocked VM details, host recovery and cache repair.

Final checks passed 203 Rust tests on Mac, 118 relevant Linux Rust tests, 121 Python
tests on Mac (two platform skips), and 133 native screenshot comparisons. Clippy
with warnings denied, formatting, diff and bundle signature checks passed. The
updated Mac app was reopened and inspected.

Normal cleanup removed the worker and its test disk, restored the original
4 GiB/two-core/128 GiB grant and left no disposable leases. Only the two permanent
VMs remain. The devbox QEMU PID stayed 3361 and T3 remained active; final host
memory health is Healthy and the pool is ONLINE. Full evidence and resolved test
findings are in `lab/evidence/memory-recovery.json`. This does not establish full
physical-host density or resolve the earlier independent Incus/QEMU incidents.


## October 29 stabilization: credentials and software deployment

Mac device keys now use the native Keychain. Seven deterministic migration tests
cover identity/pin preservation, denied and failed store operations, readback,
missing/corrupt keys, tampered references, concurrent migration, permissions and
unknown schema versions. A separate native disposable-item round trip passed.
The actual Mac profile migrated without re-pairing, its plaintext key was removed,
and the CLI, bundled GUI and SSH helper authenticated successfully afterward.

Software updates now have a dedicated local-root command, allowlisted manifest,
content/version/state-format checks, matched-file receipts, a durable journal,
root systemd pre-start recovery guards and explicit software-only rollback.
Twelve filesystem/SQLite/kernel-lock fault tests exercise the update boundary.
A managed-host regression covers separate bootstrap/runtime paths, and two more
tests prevent existing-host updates from reinstalling packages or storage policy.

Real nested tests exercised install preflight, legacy adoption, normal updates,
rollback retaining a grant acknowledged after its snapshot, SIGKILL after the
first payload replacement, and a daemon that passed help but failed service
startup. Incus and the inner VM retained their PIDs and a durable proof file;
the endpoint key remained unchanged. The updater failure did not restore an old
database. A direct managed-host software upgrade also completed successfully.

The first test exposed a genuine bootstrap/runtime filename collision: the outer
monitor refreshed `managed-host.py` inside the inner host's installed software
directory. Receipt verification blocked rollback on that drift. The outer
bootstrap now lives under `homecloud-workload`, and atomic refresh plus a
regression test protect the inner installation. A temporary compatibility
fixture allowed engine fault testing before the physical-host fix was approved;
the exact final bundle subsequently passed normal update and rollback across
multiple monitor refreshes after the fix was deployed. A new disposable worker
then installed the exact final bundle from scratch, passed receipt verification,
reported its pool ONLINE and controller active, and enrolled an independent
administrator without reusing any permanent identity or database.

With explicit operator approval, the physical host adopted receipts/guards and
installed bundle `5d862b7ffebbf55b23a946a8c4fa76c056e205fb2e817cb034cdd3aad22a58fd`.
Only Homecloud restarted. The permanent devbox retained QEMU PID 3361, its T3
service stayed active, and authenticated status plus SSH worked afterward.
A local pre-adoption recovery copy remains under
`/var/lib/homecloud/install-backups/stabilization-1793309938644877439`.
This is not an off-host backup.

Local verification passed 210 Rust tests on Mac (including 85 native GUI tests
and 133 screenshot comparisons), 125 relevant Rust tests on Linux, and 136 Python
tests (two Linux-only skips on Mac). The ignored native Keychain smoke was also
run explicitly. The existing `block` dependency still emits a future-Rust
compatibility advisory; current clippy checks pass. The build/test iteration also
caught a missing example-only tracing dependency and a Python 3.9 fixture
compatibility issue; both were corrected before the final checks.

The installer is still an explicit maintenance operation, not a transactional
hardware-policy migration. The version baseline is intentionally narrow, stable
Mac signing/notarization and permanent operator sudo cleanup remain release work,
and final-hardware density, multi-card hardware and the deferred hotspot repeat
are not proven by these deployment tests. See [roadmap](roadmap.md).

Both disposable hosts and their custom disks were removed through the normal
API. Temporary CPU/GPU/network leases are zero, the original 4 GiB / 2-core /
128 GiB grant is restored, and the two permanent VMs remain. Incus PID 3197
and permanent QEMU PID 3361 are unchanged. Structured evidence is in
[`lab/evidence/stabilization.json`](../lab/evidence/stabilization.json).

The concurrently merged Material Symbols update (`7a26e41`) was preserved.
Combined verification passed 211 Mac Rust tests, including 86 GUI tests and the
updated icon screenshot baselines. Its shared serde_json 1.0.145 pin was rebuilt and passed all 125 Linux Rust
tests. The separately approved host refresh deployed bundle
`4c0c78e64e01a255782a752fd1a9989fdf64ac188669a59caf028cb1c0fbad5c`,
then passed authenticated status and SSH with Incus/QEMU PIDs unchanged.
The earlier fault and clean-install evidence above
identifies the exact pre-integration bundle tested rather than claiming those
fault campaigns were rerun for the dependency-only rebuild.

## Storage observability transport regression

`crates/daemon/tests/transport.rs` exercises a populated storage snapshot through
a real loopback Iroh QUIC server and enrolled Rust clients. The administrator
receives SATA/NVMe identities, partitions and boot mounts, nested mirror vdevs
linked to physical disks, error counters, SMART metrics/history and independent
observation timestamps. Exact JSON comparison also checks GUIDs and SMART counter
strings beyond JavaScript's integer precision and, for counters, beyond `u64`.
The member's encoded response omits storage and its private identifying markers.
Repeated snapshots do not collect storage. An injected collection failure retains
the last observations while reporting collection errors to the administrator,
without exposing storage to the member.

The full administrator response is checked against the existing 1 MiB frame
budget. A companion test uses the shared framing helpers to round-trip a storage
payload exactly at that limit, reject an oversized outbound payload before
writing a header, and reject an oversized inbound length without a body.

These are synthetic, rootless tests, not physical disk or SMART acceptance.
Deployment fault rehearsal belongs in a manifest-owned disposable managed host
on the box, using the [existing guarded runner](../lab/README.md#software-deployment-proofs),
not the permanent controller. Physical hardware collection still needs separate
read-only acceptance; a nested host cannot prove visibility into the box's real
disk controllers or SMART support.

The matching Linux x86-64 daemon and CLI were subsequently cross-built with Rust
1.99.0 from a credential-free source snapshot. All 159 non-GUI Rust tests passed
under Linux x86-64 user-mode emulation, including the real QUIC storage tests.
The host Python suite ran 139 tests with two intentional `tarfile.data_filter`
skips. Both candidate binaries passed their help-command smoke checks. The
20-file software bundle passed the deployment engine's archive, architecture,
allowlist, syntax and per-file hash validation. Those emulated runs alone did not
constitute native Ubuntu service or privileged collector acceptance.

Read-only authenticated preflight against the physical host succeeded and reported
the pool ONLINE. The pinned independent management SSH configuration now lives at
`~/.config/homecloud/host-access/resolvr-box/ssh_config`, outside every checkout.
Its existing host-key pins, strict checking and noninteractive sudo were verified;
the original scratch copies were retired. Legacy helpers use the stable location
or an explicit override, and generated guest entries cannot overwrite the
physical host's management settings. All 21 local lab-helper tests passed,
including four access-location/pin-preservation regressions and six real-storage
checkpoint validator tests.

Read-only process baselines were captured for the controller, Incus and both
permanent VMs. Backing and capacity admitted an 8 GiB/two-core disposable host:
its 66 GiB disk charge brought committed disk to 114 GiB within the existing
128 GiB grant. Its temporary 12 GiB/three-core grant was restored during cleanup.

### Disposable native deployment rehearsal

The exact candidate bundle completed the guarded native Linux rehearsal in that
manifest-owned managed host. Its independent installation and nested KVM worked;
Incus 6.0.5, OpenZFS 2.4.1 and QEMU 10.2.1 passed the existing baseline checks.
No legacy adoption or guard bypass was needed.

- Native installer and software-deployment preflights passed.
- Upgrade with `--storage-check` verified the real root adapter, daemon cache and
  enrolled administrator snapshot before rollback: fresh boot/pool observations,
  the ONLINE managed `hcnested` pool and a linked vdev leaf. Missing smartctl
  produced explicit unavailable observations without invented SMART health.
- Software rollback retained a grant acknowledged after the deployment checkpoint.
- SIGKILL during payload replacement recovered through the real pre-start guard.
- A fixture daemon that passed `--help` but could not start was rejected; explicit
  rollback retained the controller database and restored the baseline daemon.
- The same inner QEMU/Incus PIDs, endpoint identity and durable proof file survived
  every upgrade, rollback and fault. The fixture was never run on the permanent
  controller.

Normal API cleanup deleted the worker and its owned custom volume/ZFS datasets,
restored the original grant and removed the temporary candidate. Permanent VM
states/PIDs, controller/Incus PIDs, checked installed software hashes, host
reserves and capacity matched the pre-run baseline; the physical pool stayed
ONLINE. Raw snapshots and the manifest remain private. Permanent controller
software was not upgraded.

The rehearsal also exposed an inventory classification follow-up: the logical
zvol `/dev/zd0` backing the inner VM appears alongside the boot/pool disks and
receives an unavailable SMART probe. The collector now requests lsblk's kernel
device names and excludes logical zvols before building disk inventory or
scheduling SMART probes. Five Rust regressions cover kernel-name/path detection,
preserved guest disks and hardware metadata gaps, visible unmatched vdev leaves,
and removal of cached zvol entries without consuming the bounded SMART budget.

After the fix, all 32 focused storage tests passed, alongside 165 non-GUI Rust
workspace tests on macOS and 164 on Linux x86-64 (the latter through Docker
Desktop Rosetta). The existing native Keychain test remains intentionally ignored on
macOS. Workspace clippy with all targets and warnings denied, formatting, and
diff whitespace checks passed. A fresh tracked-source-only snapshot was rebuilt
offline; the resulting Linux daemon/CLI passed help checks, and the new bundle
passed the existing allowlist, content-hash, syntax and executable-format
validator. Its source and bundle hashes are recorded alongside the finding.

The recorded rehearsal still describes the original source snapshot and bundle;
it is not an exact-artifact deployment rehearsal for this classification fix.
Refreshed native acceptance and real physical disk/controller/SMART acceptance
remain pending. The exact source snapshot, bundle digest, passed rehearsal checks
and finding's source-fix status are recorded in
[`storage-observability.json`](../lab/evidence/storage-observability.json).

### SMART review corrections

Two further review findings were independently reproduced against the local
classification fix: path-only disks could monopolize the bounded probe queue,
and a failed SMART result could disappear from the main badge behind a freshness
or probe warning. The source fixes preserve only path-based scheduling attempt
timestamps, not unidentified-device health readings, and prioritize failed-health
badges while explicitly labelling retained failures as last-sample information.

Five host regressions cover fairness across multiple batches, cache reloads
without reused readings, metadata replacement, clock rollback, and retention of
a partial failed-health observation through later skipped/failed probes. Three
native UI regressions cover partial-command errors, retained failures across
stale/skipped/unavailable discovery and probe states, and recovery after a fresh
passing sample. These initial corrections passed all 37 focused storage tests
and all eight storage UI tests.

A fresh independent review identified a remaining partial-read case: a later
temperature/metrics-only result could clear an earlier failed health result.
The parser now retains that failed observation and its original timestamp until
an explicit health result replaces it. Fresh passing or unknown health results
with partial-command errors use explicit partial-read badges rather than
misstating sample freshness. Duplicate WWN/model-serial identities now all fall
back to path-based scheduling-only caching; older caches with ambiguous
identities cannot transfer a removed disk's health to a surviving disk.

Four additional host regressions and one additional native UI regression cover
missing-health failure retention and recovery, order-independent duplicate IDs,
reordered/removed disks, legacy ambiguous caches, and stale/future partial-read
samples. The updated 41 focused storage tests and nine storage UI tests pass.

The full macOS Rust workspace passes with 269 tests, including 95 desktop tests
and the native screenshot comparisons using the suite's deterministic tiny-skia
backend. The existing native Keychain test remains intentionally ignored. All
173 non-GUI Linux x86-64 Rust tests pass through Docker Desktop Rosetta. Workspace
Clippy with all targets and warnings denied, formatting, and diff whitespace
checks pass; no screenshot baselines changed.

A refreshed tracked-source-only snapshot was rebuilt offline. The resulting
Linux daemon/CLI pass help checks, and the new 20-file bundle passes the existing
allowlist, content-hash, syntax and executable-format validator. Source, binary,
and bundle hashes are recorded under `review_fixes` in the evidence file; this
candidate supersedes both the earlier classification-only bundle and the initial
SMART-review-corrections bundle. The snapshot
includes all compiled fixes but precedes these final documentation/evidence
updates.

Neither the original native deployment rehearsal nor the earlier
classification-only bundle establishes native acceptance of the current
controller. Refreshed native acceptance and real physical disk/controller/SMART
acceptance remain pending. No permanent-host operation or deployment is part of
these local corrections.

## VM resource metrics — October 31, 2026

The native GUI now separates assigned capacity from guest CPU, available-memory
usage, root filesystem usage, and per-interface receive/transmit rates. The
unprivileged Rust controller samples authenticated, loopback-only Incus metrics
directly; no guest-runner changes or Python runtime metrics bridge are involved.
Setup, TLS boundaries, freshness, and counter semantics are documented in
[`metrics.md`](metrics.md).

Initial implementation validation passed 293 non-GUI Rust tests, 123 desktop tests including
tiny-skia screenshot regressions, and 127 Python host tests. The existing opt-in
native Keychain test remains ignored. Workspace Clippy with all targets and
warnings denied, rustfmt, whitespace checks, and signed macOS app packaging
passed. The 33 focused core/host/daemon metrics tests also passed natively on
resolvr-box.

After the other host-testing thread explicitly released resolvr-box, the installed
guarded updater accepted a controller-only compatibility bundle. It preserved the
host's existing Python guest/host adapters and privilege boundary byte-for-byte;
the unrelated Rust guest-runner migration was not deployed. Only homecloudd was
restarted. Incus and both running VM processes remained unchanged.

Live security checks confirmed a `127.0.0.1:8444` listener, a dedicated Incus trust
entry of type `metrics`, authenticated metrics HTTP 200, unauthenticated HTTP 403,
and no management route on that listener. The controller still cannot access the
Incus Unix socket. Credentials are root-owned `0640`; host policy and all control
settings other than metrics are unchanged.

A disposable enrolled Linux CLI device verified normal authenticated controller
snapshots: both running VMs reported CPU, RAM, root filesystem values and network
rates, while the stopped VM had no live usage. Guest `os.statvfs('/')` agreed
exactly with the controller during a temporary 64 MiB allocation. Used space
increased by that allocation plus ordinary background writes and returned within
68 KiB of its original sampled value after removal. Free versus available space
differed by 16 MiB, confirming that filesystem-reserved space must not be counted
as used. Existing grants, reserves and original device states were preserved.

All temporary guest files, client profiles, and the unused candidate probe binary
were removed; both disposable test devices were revoked. Root-private audit data
and the previous thread's receipts were retained. The rebuilt Mac CLI encountered
normal Keychain approval and its blocked test process was stopped without changing
permissions or adding plaintext credentials. The signed native app is rebuilt,
but live visual acceptance after reopening it and approving Keychain remains an
operator step. Automated tests, rather than disruptive live outage injection,
cover stale/failure/recovery behavior and member-scoped visibility.

Sanitized values, deployment digest, security results, disk-allocation proof, and
acceptance limitations are recorded in
[`vm-metrics.json`](../lab/evidence/vm-metrics.json).

### Independent review corrections

An independent review reproduced three blockers before PR creation: real Incus
QEMU fallback includes apparently valid CPU/memory counters, a malformed guest
label or excessive interfaces could fail every VM's scrape, and a metrics setup
failure could interrupt maintenance before workload/controller restoration.

The parser now requires a valid guest root filesystem sample, skips malformed
label/non-UTF-8 lines, scopes ambiguous or invalid measurements to the affected
VM, and bounds interfaces without rejecting healthy guests. Read-only installer
preflight validates an existing identity's lifetime and reuses custom listener
ports; subsequent metrics provisioning failure still attempts legacy guest resume
and controller restart. GUI freshness trusts the host rather than the desktop's
clock. Unobservable/transitioning VMs are not called stopped, and unknown future
metrics states decode without rejecting the full snapshot. Provisioning tests now
run in CI. Advance certificate-expiry GUI warnings remain a documented follow-up;
operators must schedule annual renewal.

A focused independent re-review found no remaining blockers. Its additional
malformed-tail identity-injection case was also fixed and regression-tested using
Incus-style label ordering, and missing identities now have an accurate diagnostic.
The documented shared-exposition provenance limitation for malicious replacement
agents remains; telemetry is informational and never an authorization, admission,
or billing input.

After these corrections, all 297 non-GUI Rust tests, 123 native desktop tests
including screenshot comparisons, and 132 Python host tests pass. Workspace
Clippy with warnings denied, formatting, whitespace/syntax checks, and refreshed
signed Mac app packaging pass. These corrections were validated locally only:
resolvr-box was released to another testing thread, so the live evidence above
describes the earlier deployment, not an exact-artifact rehearsal of the reviewed
source. No further host query, test, or deployment was performed.
