# Interface design and visual testing

## Design basis

This applies Material 3 to a desktop infrastructure app. Google's
[Expressive research](https://design.google/library/expressive-material-design-google-research)
uses color, size, shape and containment to guide attention and group related
content, while warning against replacing familiar interactions or removing labels.
Here that means tonal surfaces, rounded cards, clear primary actions and labeled
navigation. Expressive springs and shape feedback come from iced-m3's theme;
Settings exposes an app-wide reduced-motion option. UI preferences use
`directories::ProjectDirs::config_dir()` with the `dev.homecloud` namespace:
`~/Library/Application Support/dev.homecloud/preferences.json` on macOS, and
`$XDG_CONFIG_HOME/homecloud/preferences.json` on Linux (defaulting to
`~/.config/homecloud/preferences.json`). `HOMECLOUD_PROFILE` still selects only
the paired-host metadata and its associated operation journals.

On first launch without an app-wide preferences file, Homecloud imports the
selected profile's `<profile>.preferences.json`, if present, or persists defaults.
An existing app-wide file always takes precedence across profiles. The legacy
source is retained, and invalid files are reported rather than silently replaced.
An imported motion setting takes effect even if saving the new file fails, with
an error and a retry on the next launch. Preference writes are atomic; a failed
toggle save keeps the previous setting and shows an error. Pairing metadata,
Keychain references and pending creation journals are not migrated.

Related primary guidance:

- [Material typography roles](https://m3.material.io/styles/typography/applying-type)
  distinguish page headlines, section titles, body copy and labels.
- [Material color roles](https://m3.material.io/styles/color/the-color-system)
  separate surfaces and containers from high-emphasis actions and error states.
- [Material navigation rail](https://github.com/material-components/material-components-android/blob/master/docs/components/NavigationRail.md)
  keeps labeled top-level destinations available on wider windows.
- [Material common buttons](https://github.com/material-components/material-components-android/blob/master/docs/components/CommonButton.md)
  provide different emphasis levels instead of filling every action.

## Structure

VMs is the default destination. A single compact capacity surface groups
memory, dedicated CPU and disk backing, with allocation/quota numbers, progress
bars and remaining headroom. These graphics stay visible at 800×600. Click a VM's
name to open its VM; the duplicate Details button is removed.

A filled green status circle marks running VMs; a hollow gray circle marks stopped
VMs. Starting/stopping share amber. The name's tooltip provides the full state on
hover and keyboard focus. Errors, incomplete cleanup and unavailable observations
retain visible text. A consistent trailing Material icon button starts or gracefully
stops the VM. Pending work replaces its icon with an amber activity indicator and
disables duplicate requests. Reduced motion uses a stationary activity arc; its
shape does not represent a completion percentage. Lost controller connectivity
shows Status unavailable and disables lifecycle actions rather than inferring a
shutdown from disconnection. Pending feedback begins on click and survives an
accepted operation followed by an older in-flight snapshot.

Running cards keep a filled **Open terminal** button with an official leading
Material terminal icon and its text label. Cards show configured resources,
concise T3 readiness, and another owner's name when appropriate. GPU claims and
remembered preferences remain distinct. All icons, including the sidebar, cloud
mark, terminal/play/stop controls and contextual FABs, come from iced-m3's official
offline Material Symbols catalog. They use the Outlined family, with filled
artwork for selected navigation destinations. Terminal buttons use 20px optical
artwork at 20 logical pixels; other icons use 24px artwork and layout. Google's
license and attribution notice are included in the Mac bundle.

The VM page's pinned header shows name, lifecycle state, host and terminal/Start/Stop
controls. Secondary **Overview** and **Settings** tabs organize content within
this VM, following [Material tabs guidance](https://github.com/material-components/material-components-android/blob/master/docs/components/Tabs.md).

Overview shows configured resources and a compact T3 application card: connection,
Codex and Claude accounts, and GitHub access for Git and gh in the VM. Only unfinished setup expands into browser/code steps.
Immediate preparation, waiting and verification states remain visible; successful
sign-in produces an explicit completion notice before returning to compact account
rows. Settings contains resource resizing, GPU preference, technical disclosures
and deletion. There is no nested Manage destination. Create/resize use focused
forms; permanent deletion requires its own confirmation.

Remembered GPU preference is distinct from ownership. Starting with an unavailable
GPU opens a blocking choice: Go back, or explicitly Start without GPUs. Going back
closes only the confirmation and keeps the VM stopped. The latter keeps
the preference and makes a fresh admission request. A fully stopped VM shows its
configured size while account meters count only held compute claims; an incomplete
stop retains the explicit Finish stopping control.

Hosts opens a directory of machines. Each card summarizes physical hardware,
storage health and backed resources against VM capacity; its full name row opens
the host. The current controller supplies one authenticated host. Fleet discovery
and aggregation remain backend follow-ups; hostnames are display labels, not identities.

A host detail view has one pinned row of Overview, Allocations and Hardware tabs.
Overview uses one stable identity color per user, with stronger fill for held VM
claims, softer fill for unused grants, and neutral fill for unissued capacity.
Small CPU pools use individual physical-core tiles; larger pools use proportional
blocks. Selecting a person highlights their share without changing the scale.
These are native iced widgets using Material tonal color pairs, not web content.

Allocation graphics use the existing snapshot and do not change admission or
storage policy. Backing is summed per user as `max(grant, committed)` in each
dimension, including disabled accounts and commitments above a reduced grant.
Allocations shows one card per person with memory, CPU and storage side by side,
allocated/grant values and unused amounts. There is no resource-tab row. Expand
Show VMs for that person's per-VM claims; admin Edit grant remains in the card header.
Members see only their own grants and machines. Live CPU/RAM
utilization and per-user physical ZFS consumption are not yet available and are
never inferred from these guarantees. ZFS pool availability is shown separately
in Hardware, not presented as unissued capacity.

Administrators also see Host headroom in Overview: memory/cores outside
the VM budget, and the configured pool free-space floor. Outside-budget capacity
can include host services, safety margins and unmanaged workloads; it is not
claimed to be an exclusive host reservation. The disk floor is an operating policy,
not a ZFS reservation. Older hosts that omit this metadata show Not reported;
the UI never subtracts current ZFS free space to invent a reserve. Admin Edit reserves presents total system memory, physical CPU cores and the pool
free-space floor, with sliders and exact numeric inputs bounded by explicit
minimums and existing user promises. The preview shows resulting VM budgets.
Lowering reserves releases unissued capacity; increasing them cannot reclaim
unused grants implicitly. Frozen draft ranges and retained exit geometry keep
refreshes and closing animations stable. The server rechecks revision/admission.

A separate ZFS cache section offers Automatic (Homecloud) and a custom maximum.
The custom slider stays in place, disabled during Automatic, so changing mode
keeps dialog geometry stable. Automatic previews its effective ceiling. Cache
settings stay within the displayed system allowance and do not alter user grants.
A larger cache allowance requires a larger total system reserve. The editor and
host summary show the operating floor and cache allowance explicitly. Admin Host headroom
shows configured system reserves plus current ARC size, effective minimum/maximum
and selected mode. Cache temporarily above a lower applied maximum is explained
as awaiting reclamation. A reserve release stays in progress until reclamation is
confirmed, with conservative capacity and an explanation of the wait.

Accepted changes immediately show progress and preserve conservative capacity
through stale refreshes. Completion is announced. Interrupted changes offer Retry;
Discard unapplied change requires a root tombstone proving no effects began.
Members do not receive reserve metadata or controls.
See [reserve policy](mvp.md#admin-host-reserve-editing).

GPU overview tiles resolve durable claims before hardware readiness, so a device
being provisioned or released cannot look available just because inventory says
ready. A stopped VM's remembered GPU preference does not constitute a claim.
GPU setup readiness is distinct from storage health. Hardware contains processor,
memory and storage inventory plus passthrough setup; PCI addresses, drivers, IOMMU
evidence, and host-display warnings sit behind a diagnostics disclosure.

People contains user grants,
invitations and device revocation; ordinary users do not get this destination.
Activity presents requests by action and machine name with visible progress and
an explicit administrator retry for operations requiring review. Failed requests,
including superseded shutdown attempts, remain labeled Failed with their original
error. Only applying/retrying phases appear In progress; unrecognized phases show
Status unknown. Allocation rows use the same lifecycle state as the VM page,
including unavailable observations, retained cleanup claims and blocked starts.

The narrow layout condenses resource summaries and uses one VM column. Wider
windows use two columns. Content reserves a scrollbar gutter; forms keep their
actions visible when scrolling. Invitations remain hidden from screenshots.

## Run screenshot tests

```sh
sh scripts/test_ui.sh
```

These tests render the **same native iced view tree** as the app through
`iced_test::Simulator`. They use the software tiny-skia renderer, bundled Roboto,
fixed fixture data, and reduced motion. They never load a real profile, connect to
Iroh, enroll a device, or mutate the Linux host. They cover 68 screens/states,
including 800×600, 1180×820 and 1440×900 windows, a member account, empty and
over-quota states, graphical host allocations, owner highlighting, members,
zero-capacity and larger-core pools, GPU release, hardware setup, Settings, and the
principal dialogs.

Baselines live in `crates/desktop/tests/screenshots/`. Every run writes fresh
actual images to `target/ui-screenshots/`. A changed pixel fails the test and
produces a highlighted `*-diff.png`. Missing baselines fail too; a normal test
run cannot silently approve new output. The macOS CI job runs the comparisons
and uploads failure artifacts.

For an intentional visual change:

```sh
UPDATE_SCREENSHOTS=1 sh scripts/test_ui.sh
```

Open the generated images and inspect layout, clipping, hierarchy, contrast,
button labels and dialog footers before accepting the baseline changes. Then run
the command again **without** the update flag. Exact pixel comparisons are pinned
to this renderer/font combination; investigate platform rasterization differences
before treating a CI mismatch as a design change.

Interaction tests also exercise navigation, dialog cancellation, resource prefills,
member navigation permissions, host tab navigation, owner selection, ledger grant
editing, and target-specific management actions. Accounting checks cover retained
backing after quota reductions, pending claims, zero/oversized capacity, and GPU
claim precedence over inventory. Pixel
tests do not replace native macOS checks for window chrome, clipboard integration,
Terminal automation, VoiceOver or OS-level accessibility behavior.


Application setup runs on the unified VM page. Expanding Advanced, closing
the window or returning to VMs leaves its guest operation running. The
browser action is explicit and expires; a pending challenge stays scoped to its
operation and current owner. Authorization input is masked and cleared on submit
or navigation. New VM offers an optional T3 installation switch. See
[managed apps](managed-apps.md).

Screenshot cases include ready/setup/interrupted/provider states, stopped and
cleanup states, resource/GPU settings, long names, member permissions, and scrolled
800×600 forms. Interaction tests verify the clickable name, targeted header
controls, Advanced expansion without canceling sign-in, masked return-code input,
completion feedback and separate deletion/GPU confirmations. These are real iced
renders; fixture accounts and codes are synthetic.

VMs card tests also check selected-machine terminal/Start/cleanup actions,
GPU consent on Start, disabled duplicate requests, review routing, and T3 readiness
only after both connection and provider authentication are observed. Screenshot
fixtures include ready T3, another owner's VM, allocated/remembered GPUs, long
names, cleanup, pending updates, and intervention states.


## Explicit shutdown recovery

A graceful shutdown that cannot complete becomes a reviewed operation. The VM
page offers Keep waiting (another graceful attempt) and Force stop…
with its own confirmation describing lost unsaved work and pending writes. Force
stop is also available under Advanced → Power recovery when the operation lane is
free. It is never an automatic timeout fallback. A VM that is already off but still
holds compute offers Finish stopping, which retries cleanup without forcing it.

The backend authorizes the owner or an administrator, preserves the old operation
and its error in history, and serializes the new request through the same durable
VM operation lane. Active operations and unrelated reviewed operations cannot be
superseded. CPU/RAM/GPU claims survive until the host explicitly confirms shutdown
and cleanup. See [operations](operations.md).

Interaction coverage includes Tab/Enter activation, focus tooltips, cancel and
VM-specific force confirmation, graceful retry, immediate/stale-snapshot progress,
and unavailable status. Screenshots include both transition states, hover hints,
shutdown recovery, and 800×600 confirmation. Keyboard behavior is tested in the
native iced view tree. Native macOS VoiceOver semantics remain an existing iced
integration limitation and are not implied by these tooltip checks.


## Unified VM and account visibility

The entire card name/subtitle header is a single, generously sized rectangular
hit target, separate from the trailing lifecycle control. Graceful Stop opens a
VM-specific confirmation; Cancel sends no mutation. Finish stopping still retries
cleanup directly when the guest is already off. Force stop retains its separate
warning and confirmation.

Overview and Settings are merged. The VM page has one Resources card with
a resource summary, followed by Apps & accounts. The bottom-right pencil + Edit VM
FAB opens one editor for name, CPU, memory, disk and GPU. Technical details, power
recovery and deletion live under Advanced. Toggling Advanced retains pending
sign-in and masked, unsubmitted return-code input; leaving the page clears input.

Compact dots replace account/status chips. Green T3 means a saved account link;
green Codex/Claude/GitHub means their CLI reports a saved sign-in. Amber is an operation
in progress; hollow gray denotes unavailable or unauthenticated state, with
visible explanatory text. Hover hints explain green status. The aggregate Ready
to use badge was removed: authentication does not prove remaining allowance or a
successful provider request. VM lifecycle status remains independent.

Owners see supported account email and subscription/plan data. Provider usage
management stays in T3; Homecloud has no allowance bars, usage links or background
usage polling. A healthy T3 connection uses the status dot and its tooltip without
a redundant linked-status caption. Setup instructions and connectivity warnings
remain visible. Current status JSON exposes an account ID without email; this
does not indicate incomplete setup. Its technical ID remains under Advanced. Copy is
device-neutral. Other administrators can inspect operational state but do not
receive account display metadata in snapshots.


Apps & accounts uses a general card heading with consistent T3 Code, Codex,
Claude and GitHub rows. GitHub follows the AI providers and is not counted as one. Spacing separates the rows; there is no divider between the T3 and
provider accounts. The T3 row's dot/tooltip communicates its saved link status;
setup progress and connectivity warnings retain visible explanatory text.


## Edit VM and restart consent

Edit opens a single staged form for the display name, configured memory/cores/disk
and GPU selection. GPU configuration moved out of Advanced; the header button uses
the official Material edit symbol plus text. Save is disabled when unchanged or
invalid. Disk shrinking remains unsupported. GPU and CPU-only VMs share
the normal 1 GiB RAM minimum. Each physical GPU has a checkbox; several may be
selected, and all unchecked means no GPUs. There is no duplicate Keep/No GPU choice. An unchanged
selection preserves the next-start preference internally, including a VM
started without its remembered GPUs. That special case offers a separate explicit
Forget remembered GPUs action. Unavailable existing selections can be removed;
a stopped VM may remember cards whose availability is checked on its next start.

The form keeps the same geometry while editing; validation has a fixed helper
area. Save changes becomes Save & restart… when running resources would change.
There is no changing restart heading or explanation above the form. The button opens
a second confirmation warning about interrupted tasks and disconnected terminals/
T3 sessions; Go back retains the draft. The confirmation submits exactly the
reviewed draft for the selected VM. Errors appear near its heading. The server
also checks current state and restart consent, so a stale client cannot cause an
unconfirmed restart.

Renaming updates the control-plane display name without provisioning calls. It
keeps the immutable VM ID, Incus/SSH identity, network slot and managed application
records. Resource changes are one durable operation; increased capacity and any
added GPUs are claimed together before host work begins. The stopped form
saves GPU preferences without claiming them; the existing unavailable-GPU decision
still occurs on Start. Force stop and deletion keep separate confirmations.


## Resource sliders and inline provider identities

Create VM, Edit VM, coworker invitations and resource grants share
one stepped iced-m3 slider control per resource. Each slider has a compact numeric
field for exact entry. Memory/disk use whole GiB and CPU uses whole physical cores.
Small ranges show ticks; large ranges keep a clean track. Keyboard arrows/Home/End
work alongside dragging and typing. All controls edit a draft until Save.

Edit disk ranges start at the existing logical size. Typed shrink requests are
invalid and cannot be saved. Existing allocations above reduced quotas remain
visible and can be reduced; opening Edit never clamps them. CPU/RAM for stopped
VMs can be configured for a future start, where availability is checked again.
Create uses uncommitted quota, edits reuse existing backing, and grants account for
other users' promised capacity or retained commitments, including disabled users.
New forms choose defaults within their ranges; zero grants are allowed.

Ranges are captured when opening a form so background refreshes do not move the
slider handles. Local validation does not reserve resources; the server rechecks
admission on Save. Storage ranges include the controller-reported per-VM disk
overhead, exposed as `Snapshot.disk_overhead_gib_per_vm`. Older snapshots default
to the previous 2 GiB overhead. Validation has a fixed helper area; restart consent
still uses Save & restart… and its reviewed confirmation.

Signed-in Codex and Claude rows show the provider name, status dot and subdued
email/plan on one line when they fit. Long metadata wraps below at narrow widths.
Provider credentials and usage are unchanged; metadata remains owner-only.

A signed-in GitHub row shows `@login · Git and gh in this VM` and, for the owner,
an inline **Remove access…** that opens a confirmation dialog. The dialog states
that repositories and commits in the VM are kept and that the token stays valid
on GitHub until revoked there. While removal runs the dot turns amber with a
loading line. The expanded sign-in flow is the only place that notes the GitHub
connection used by Codex's plugin is separate.


## Contextual VM floating actions

VMs is the page, navigation and list heading. Visible titles, forms and messages
consistently use VM terminology.
New VM is a bottom-right extended iced-m3 FAB with the official Material add icon.
The same position on an individual VM holds Edit VM with the pencil icon.
Each page has one floating action; the old header/empty-state duplicates are gone.
The VM header retains an outlined Open terminal action and its power control.

FABs stay outside the page scroller, with equal 24px bottom/right edge insets.
Header actions and body cards share that right edge; the scrollbar occupies a
separate 16px gutter farther out. Bottom content padding
lets the final card, footer or advanced action scroll above the FAB with a 16px gap.
Both use the native extended FAB's focus, keyboard activation, elevation and state
layers. Existing busy, offline and VM-operation guards still apply. Modal dialogs
block the background FAB throughout entry/exit without changing the outgoing layout.


### RAM capacity review and host workloads

Admins see fresh physical RAM separately from reviewed VM capacity. A detected
change opens a review showing the proposed envelope, user promises and held
claims. Adoption preserves those records. Deficits explain how to stop, resize
and reduce grants; pending adoption offers retry and confirmed-unapplied cancel.
Ordinary users cannot edit reserves or adopt capacity.

Create VM chooses one workload before resource controls: Ubuntu, T3 Code, or the
admin-only Homecloud host lab. Host labs include a separately backed 32 GiB test
disk and show their controller, Incus and ZFS readiness in the existing VM details
page. They have no T3/provider pairing controls.

## Host memory warnings and recovery

The authenticated snapshot carries a typed memory-health condition to every user.
Only administrators receive aggregate promise/claim quantities and reserve policy.
A non-dismissible warning remains above the page body across navigation, including
VMs, Hosts and Activity; it clears when a fresh verified condition is healthy.
An offline client identifies a retained warning as the last verified state.
This is a persistent condition display, not email/push or an acknowledgment log.

The host directory says Check host for memory-policy problems even if the pool is
ONLINE. Review host opens the host Overview and its Memory recovery section, with
budget, promises, held memory and links to affected VMs, grants, reserves/cache and
physical RAM review. Lowering a grant alone does not release a VM's claims.

A held VM waiting on host recovery shows Startup blocked instead of an activity
spinner. Its details explain the host condition and keep Stop and release compute
behind the ordinary confirmation. A fully stopped VM's Start is disabled when a
fresh claim would be unsafe. Existing running VMs retain terminal access. Ordinary
users see their own VMs and an administrator-help instruction. An effective ARC
mismatch permits explicit reapplication of the existing cache policy from the
reserve editor; inspection never performs that repair.

The reserve editor uses the smaller reviewed/physical RAM envelope during a
shortage. Overlarge memory promises contribute zero extra reserve headroom and do
not prevent opening the editor for recovery. Existing total reserves remain
editable downward; no new unpromised capacity is invented by the form.

## Physical host shutdown

Administrators on a capable host use **Hosts → select host → Shut down host…**.
The confirmation names the host, counts affected running VMs, explains that
terminal/T3 sessions and running tasks will be interrupted, and explains how the
host can be powered back on. Cancel dismisses the dialog without submitting work.
Confirmation uses one request key for retries and rejects a changed kernel boot.
Older hosts and the simulator do not advertise this action.

An accepted request closes the dialog and shows **Shutting down VMs…**, then
**Powering off host…**. The progress banner follows the user across pages;
workload changes are disabled while shutdown is active or awaiting recovery.
After connectivity is lost, **Host unreachable** explicitly leaves the final
power state unverified. Disconnection is never presented as confirmed power-off.

If a guest or final verification fails, the host stays on and the administrator
can retry shutdown or **Cancel shutdown and resume VMs**. Cancellation explicitly
resumes VMs with retained Running intent, including guests already drained.
Once power-off has been attempted, an absent reply cannot prove cancellation is
safe: recovery offers review/retry, and cancellation is unavailable.

Status refreshes carry request generations so a later shutdown by another
administrator replaces an earlier accepted/cancelled receipt. A response from
before acceptance cannot erase that pending feedback. During draining, only the current guest shows Stopping; queued guests retain
their observed state. Read-only observations refresh guest labels during review,
so an out-of-band stop or restart is reflected without resuming workload changes.
A power-off request that leaves the host reachable for one minute offers explicit
review/retry without claiming cancellation is safe. Device revocation and account
security actions remain available while VM/resource changes are paused.
