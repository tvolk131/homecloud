# Managed guest applications

A VM remains an ordinary resource-managed VM. `VmSpec.applications` selects
optional software at creation; existing VMs can add an application afterward.
The supported first adapter is T3 Code 0.1.2 on the Ubuntu x86 guest image.
The old host-wide `t3_version` setting no longer selects guest software.

Each `ManagedApplication` records its VM, application kind, desired version,
boot-start policy, and latest observation. Application operations have their own
SQLite table and unique active-operation constraint; waiting for browser approval
never holds the VM's resource-operation lane. Concurrent installation/setup
requests for the same actor and action reuse the existing operation. Different
actions cannot share an idempotency key or overlap within one application.
Installation and T3 connection permit owners/administrators; personal provider
and GitHub sign-in/removal and their browser challenges are restricted to the VM owner.

The adapter has three hooks:

- `ensure_installed`: bring a pinned installation and its boot service into place.
- `inspect`: return service/setup observations and any active guest-job result.
- `start_setup(action)`: request a compiled-in named action, currently `connect_t3`,
  `sign_in_codex` / `sign_in_claude`, or `sign_in_github` / `sign_out_github`.
  These are fixed adapters, never caller-supplied commands.

The controller runs an independent application worker. Short fixed Incus commands
launch/query a transient systemd service inside the guest. Installation and
interactive authorization continue when the GUI closes or the controller restarts.
There is no network listener or permanently running Homecloud guest daemon.

The guest job runs as `dev`; the installed runner is root-owned. It uses T3's own
installer, background service, and T3 Connect CLI. The bundle also installs native
Codex CLI 0.165.0 and Claude Code 2.1.308, within T3's bundled compatibility ranges,
plus GitHub CLI 2.103.3. The Codex release archive is installed with its code-mode
host and ripgrep sidecars; Claude uses its vendor's native installer layout; gh's
release tarball is unpacked into a versioned directory. All initial downloads are
SHA-256 verified before execution/extraction. Existing compatible versions
are retained: a system gh at 2.40 or newer (for example Ubuntu 24.04's package)
is used as-is, while an older one is shadowed by the pinned copy in `~/.local/bin`.
Node/npm are not required by these native distributions.
Optional runtime dependencies are installed before T3 starts. T3's persistent user service is enabled at boot
with lingering, whether or not account connection has finished. VM stop/delete
remains available and interrupts guest jobs through normal shutdown.

## Authorization and recovery

`start_setup(connect_t3)` invokes `t3 connect link --headless` in a supervised terminal. The
adapter recognizes the pinned CLI's relay-download confirmation and device
challenge. It approves only that known relay-install prompt. URL/code parsing is
specific to this release; a changed output format requires adapter validation.
The displayed expiry is conservative because the CLI rounds its lifetime to
minutes. Never log the CLI's complete output.

The browser URL and user code live in private guest job state. T3 challenges are limited to owner/admin
snapshots; Codex challenges are limited to the VM owner. It is cached transiently in the host
controller and explicitly excluded from the host database, audit records, and
ordinary CLI `status` output. OAuth device secrets and durable T3 credentials stay
inside the guest. Homecloud's device authorization and T3 account authorization
remain separate: disabling a Homecloud account does not automatically revoke an
already established T3 Connect identity.

- GUI/controller restart: rediscover the existing guest unit and valid challenge.
- VM reboot or dead unit before credentials are saved: mark the attempt interrupted,
  clear the old challenge, and offer a new-link action. Do not generate endless
  replacement codes while nobody is around.
- Credentials saved before interruption: inspect actual T3 state and resume the
  remaining link/service activation with the same logical operation.
- Linking succeeded before the host recorded it: recognize persisted success,
  including a previously interrupted operation for that same guest job.
- Revoked credentials: require explicit user authorization; never silently relink.
- Expiry/denial: finish the attempt with a retryable message; retain the installation.
- Network trouble: preserve account state and distinguish it from remote reachability.

After linking, restart T3 once to activate its connection. This can interrupt any
existing T3 work, so initial setup is intended for a fresh backend. T3 then owns
its persistent service and managed tunnel; the Homecloud GUI is not its transport.
An explicit installation repair restarts an already-running T3 service only when
its process environment lacks `~/.local/bin` on PATH, or when T3 itself was
installed by that run. Adding a new tool (such as gh) to an existing VM therefore
does not interrupt T3 sessions. Fresh installation starts it once. Codex sign-in
is available in Homecloud using the guest CLI’s structured app-server device-code
flow. Claude sign-in uses its guest CLI’s browser flow, with a private return-code field in Homecloud.
Homecloud reports provider binary versions separately from account pairing and
exposes an Install provider tools action when they are missing. A systemd user
service drop-in makes `~/.local/bin` available at boot without an interactive shell.
Use at least 4 GiB RAM for a T3/Claude devbox, matching Claude's supported baseline.

Upgrades, uninstall, custom scripts, application catalogs, and guest-data deletion
are not part of this first interface.

## Operator usage

In **New VM**, select **Install T3 Code**. Once the VM and installation are ready,
open the workspace’s **Details → Overview**, choose **Connect T3 account**, and open the authorization page.
For an interrupted attempt, choose **Get a new authorization link**. Closing the
window leaves the guest job running. After approval, sign into the same T3 Connect
account in the Mac T3 client and select the environment. Choose **Sign into Codex** in Homecloud, then **Continue in browser**,
and approve with your ChatGPT account. Refresh provider checks in the remote T3
environment afterward. For Claude, choose **Sign into Claude**, open the page, then return its authorization code to Homecloud and choose **Complete Claude sign-in**. Never send this code in chat.
For Git and gh inside the VM, choose **Sign into GitHub**, open GitHub's device
page, sign in with the intended account and enter the displayed code. **Remove access…**
signs the VM out after a confirmation.

CLI equivalents:

```sh
homecloud create devbox --memory 2 --cores 1 --disk 20 --ssh-key ~/.ssh/id_ed25519.pub --t3
homecloud app-install VM_UUID
homecloud app-setup APPLICATION_UUID
homecloud app-setup APPLICATION_UUID --codex
homecloud app-setup APPLICATION_UUID --claude
homecloud app-setup APPLICATION_UUID --github
homecloud app-setup APPLICATION_UUID --github-sign-out
homecloud status
```

Ordinary SSH access continues through Homecloud/Iroh with pinned guest host keys.
T3 Connect uses its own outbound managed connection; no public guest listener is
opened by this integration.

## Codex account setup

Codex 0.165.0's generated schema and real stdio handshake were validated against
`account/read`, `account/login/start` (`chatgptDeviceCode`), and correlated
`account/login/completed` notifications. Codex owns OAuth, its credential store,
and token refresh; Homecloud never reads/copies the credential file or supplies
external tokens. Account observations expose only `signed_in`, `signed_out`,
`unknown`, or `unavailable`, without email/plan/token details. `signed_in` means
Codex reports saved credentials, not that a paid model request has succeeded.
Existing accounts are retained rather than replaced by another login flow.

The supervised guest job holds the login process, not the GUI or controller.
The challenge goes only to the VM owner and is excluded from the host DB, audit,
and CLI status. Only OpenAI’s exact device verification URL is accepted. JSONL
frames, notification queues, command deadlines and process cleanup are bounded;
raw upstream errors/stderr never enter observation messages. Device-code login
may need enabling in ChatGPT Settings > Security or by a workspace administrator.

Homecloud limits each attempt to ten minutes, independently of any earlier
upstream expiry. Closing the GUI/controller leaves the same attempt running;
VM reboot/dead job clears an unfinished challenge and requires explicit retry.
Saved Codex credentials repair a matching interrupted Codex job. T3 being paired
cannot complete a Codex job, and failed provider auth never unpairs T3. Inspect
only observes accounts; it never starts login. Fixed guest-runner upgrades are
published atomically on inspection without reinstalling/restarting T3.

Official protocol: https://learn.chatgpt.com/docs/app-server

## Shared settings and setup progress

Workspace **Details** is the single destination for VM management. The shared
header keeps Start/Stop and terminal access available above **Overview** and
**Settings** tabs. Overview groups T3 connection and AI provider accounts within
one application card. Each provider has its own status, action, retry/error copy,
and expanded browser interaction; successful sign-in returns to a compact row
and produces a completion notice. One authenticated provider is enough.

Settings holds resource resizing, GPU preference, technical details and deletion.
Resize and destructive/conflict confirmations remain focused dialogs. Versions
and boot-service details are collapsed in Settings. The active provider flow is
brought into view on submission and when its challenge first arrives on Overview;
switching tabs never cancels the guest job. Unsubmitted authorization input is
cleared when leaving the flow. Returning observes the same operation rather than
creating a new one.

The desktop shows Preparing immediately on click, retains the returned operation
across older in-flight snapshots, and merges it with later observations. Guest
observations correlate `operation_id` with `operation_stage` (`preparing`,
`awaiting_approval`, `verifying`, `complete`). Challenges from another operation
cannot populate the current interaction. Waiting for browser approval uses stable
copy; active preparation/verification uses the iced-m3 loading indicator with
reduced-motion support. No progress percentage is invented.

## Claude account setup

The Claude 2.1.308 spike used an isolated temporary guest HOME and confirmed
`claude auth login --claudeai` emits a `https://claude.com/cai/oauth/authorize`
URL and waits at a Paste code prompt. The adapter validates that exact HTTPS
host/path and code/PKCE parameters. Terminal OSC 8 hyperlink frames are parsed
separately from their visible labels, with BEL and ST terminators supported. A
hidden target must never be joined to the visible URL. Incomplete frames are
ignored until complete; URLs containing control characters are never forwarded.
Shared terminal fixtures exercise both guest parsing and the desktop browser
guard, which continues to reject control characters. Identical duplicate OAuth
values are tolerated, while conflicting values are rejected. An unexpected
authorization page fails explicitly rather than leaving setup silently pending.
A wide guest PTY avoids wrapping the URL;
raw terminal output is bounded and never logged. `claude auth status` JSON is
normalized to account presence and allowlisted account display metadata.
Existing signed-in accounts are retained; unknown status blocks replacement login.

`SubmitApplicationCode` accepts a redacted code value for an exact application
and operation. Current owner authorization, running VM, waiting Claude operation,
matching observation and live private challenge are all checked. The code is
passed through fixed helper stdin, then a private guest FIFO to the original CLI;
it is never stored in the host database, audit body, guest job JSON or command
arguments. The guest rechecks operation ID, boot ID, phase, expiry and FIFO
ownership/type. A short delivery lock prevents concurrent submissions from
injecting duplicate codes. The input is cleared immediately from the GUI on send
or when its challenge ends. Closing the GUI or restarting the controller leaves
the CLI/FIFO alive; a dead job requires explicit retry. No T3 restart is needed
for provider authentication or fixed runner upgrades.

CLI prompt parsing remains tied to the validated Claude version. Browser approval
and a real Claude provider task require owner-assisted acceptance after deployment.


## GitHub CLI access

GitHub sign-in is optional, owner-only and separate from the AI providers: it
never satisfies "Sign into at least one provider". The row's caption shows
`@login · Git and gh in this VM` when signed in.

gh 2.103.3 was validated without a terminal: `gh auth login --web` prints the
one-time code and `https://github.com/login/device` and waits, without prompting.
With a terminal it stops at interactive questions, so the adapter uses plain pipes.
Only complete output lines are parsed; only that exact device URL with a
`XXXX-XXXX` code becomes a challenge, and anything else fails explicitly.
Raw gh output is bounded and never forwarded.

The job runs, in order:

1. Refuse to replace an existing account; unknown state blocks login.
2. `gh auth setup-git --hostname github.com --force`, before any token exists, so a
   Git configuration failure leaves nothing behind. It writes gh's credential
   helper for github.com only.
3. `gh auth login --hostname github.com --web --git-protocol https --skip-ssh-key
   --insecure-storage` with default scopes (no `workflow`) and non-expiring
   tokens (`--expiring` would want Git 2.46; the guest has 2.43).
4. Confirm the saved login, then best-effort Git author setup: when `user.name`
   or `user.email` is unset, fill it from `gh api user` using GitHub's
   `ID+login@users.noreply.github.com` address. Values the owner set are never
   overwritten; values Homecloud wrote (recorded in
   `~/.local/state/homecloud/github-identity.json`) follow a later account.

gh stores its token in `~/.config/gh/hosts.yml` (mode 0600; the guest has no
keyring daemon). Homecloud never reads it. Token environment variables
(`GH_TOKEN`, `GITHUB_TOKEN`, Enterprise variants and `GH_HOST`) are removed from
the adapter's environment so they cannot hide or replace the stored sign-in.
Git and gh inside the VM, including T3 tasks, get the account's full repository
access, which the expanded flow states before the owner approves.

`sign_out_github` runs `gh auth logout --user` for each saved github.com account.
It does not revoke the token on GitHub; the confirmation dialog points to GitHub
Settings → Applications → GitHub CLI, noting that revoking there also signs out
other devices using the same OAuth app. Deleting the VM likewise leaves the token
valid until revoked. Interrupted sign-in or removal recovers only when the
observed account matches that action's own outcome.

References: [gh auth login](https://cli.github.com/manual/gh_auth_login),
[gh auth setup-git](https://cli.github.com/manual/gh_auth_setup-git).


## Provider-connected services are a separate account scope

A Codex session can expose an OpenAI-connected GitHub plugin even when the guest
has no `gh` executable, GitHub token environment variable, Git credential store,
or outbound SSH key. A live owner report was traced to the plugin's
`github_get_user_login` tool. The same plugin identity was observed in both the
Mac and guest sessions, while the Mac's independent GitHub CLI used a different
account. This was not a GitHub login created by Homecloud provisioning.

Provider sign-in can make already-authorized external app capabilities available.
Those services' credentials/account choices may be managed outside the guest in
the user's OpenAI account or workspace. Changing `git config` or `gh auth` in the
VM does not switch that plugin identity. Manage the relevant GitHub connection in
ChatGPT Settings > Plugins > GitHub, select the intended external account, and
verify the plugin identity again. A workspace-managed connection may require its
administrator. Changing the connection can affect other sessions that use it.

Homecloud's optional owner-driven GitHub CLI sign-in (below) establishes
guest-local Git/`gh` access and, when unset, Git commit author settings. Fresh VMs
remain free of operator GitHub credentials. The interface never presents it as
replacing or controlling the provider's separate GitHub plugin connection; the
expanded sign-in flow says so explicitly.

Reference: https://help.openai.com/en/articles/20001494-connecting-and-managing-app-accounts-in-chatgpt


## Apps & accounts interface

The workspace card is titled Apps & accounts, with rows for T3 Code, Codex,
Claude and, when the runner reports the GitHub CLI component, GitHub. It currently presents the T3 managed application and its provider-tool
components; these rows are not independent managed-application records. The
shared heading leaves room for other managed tools while setup behavior remains
specific to each adapter. VM power and resources stay in their own controls.

## Read-only account identity

`ApplicationObservation.accounts` carries optional, generic account display data:
component name, email, username, account ID, plan, authentication kind and observation time.
Provider components retain their existing installation/authentication status. No
credential or raw provider reply belongs in this model. The daemon clears account
metadata from snapshots for everyone except the VM owner, including other admins.

The guest adapter queries Codex app-server `account/read` with refreshToken false
and Claude `auth status` JSON. GitHub reads only gh's saved active login locally
(`gh config get --host github.com user`), never a token and never the network,
so the regular inspection loop spends no GitHub API rate limit. Only allowlisted, bounded display strings escape
normalization. Account status caches for 10 seconds. Fresh guest observations
contain no provider usage data, and no adapter requests Codex rate limits.
Historical usage fields in stored observations are ignored during deserialization;
legacy guest usage-cache files are unused. No SQL migration is required.

Homecloud prepares the VM and provider accounts. T3 owns ongoing provider usage
management, so Homecloud has no allowance bars or external usage-page link.
Claude's documented status-line usage fields may be useful elsewhere, but no
status-line collector or provider usage prototype is planned for Homecloud.

T3 0.1.2 `connect status --json` reports cloudUserId without email. The healthy
connection row shows its status dot/tooltip without a redundant “Linked to T3”
caption. Setup progress, sign-in instructions and connectivity warnings remain
visible. Missing email does not indicate incomplete setup. Homecloud does not
guess identity from a browser session or parse tokens.

Fixed helper upgrades install account_metadata.py on host and guest without
reinstalling T3 or restarting its service. Inspection neither logs in, replaces
accounts nor issues inference requests. Account changes remain owner-driven.

References: [Codex app-server](https://developers.openai.com/codex/app-server/),
[Claude CLI](https://code.claude.com/docs/en/cli-reference), and
[T3 remote access](https://github.com/pingdotgg/t3code/blob/main/docs/user/remote-access.md).


## TODO: T3 Connect account identity

- [ ] Upstream T3 CLI: expose the optional saved account identity in
  `t3 connect status --json`, without returning credentials or starting a sign-in
  flow to inspect it. Test absent identity and preservation across credential
  refresh. Keep email distinct from username/account-ID fallbacks.
- [ ] Homecloud: consume the supported identity field after an appropriate CLI
  release, retain owner-only account visibility, and show useful account text
  when available. Keep the healthy row compact when identity is absent.

T3 v0.1.2 already stores an optional identity label and can print it during
`connect login`; status JSON omits it. This follow-up extends that supported
interface rather than reading private credential storage or repeatedly invoking
login. It is a recorded TODO, not an upstream issue or PR submitted yet.

Source: [CLI status/login implementation](https://github.com/pingdotgg/t3code/blob/v0.1.2/apps/server/src/cli/connect.ts)
and [saved identity handling](https://github.com/pingdotgg/t3code/blob/v0.1.2/apps/server/src/cloud/CliTokenManager.ts).


## Managed Homecloud host

`homecloud_host` reuses this application lifecycle with fixed root installation
and read-only inspection, and has no account setup/browser hooks. Its system
services recover on boot before any client pairing. It requires an admin-created
VM with an independently backed test disk. See [the full host lab](managed-host-testing.md)
for its bundle, ownership fences, lifecycle and external test supervisor.
