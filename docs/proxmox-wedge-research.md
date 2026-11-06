# Proxmox-backed Homecloud: product-wedge assessment

Research date: November 2, 2026.

## Verdict

There is a plausible narrow product wedge, but no demonstrated exclusive
capability or validated market yet. Proxmox backing is an engineering and
distribution choice, not the differentiation itself.

The hypothesis worth testing is:

> Turn a small team's existing Proxmox machine into managed, persistent T3
> workstations: invite someone, allocate a safe budget, connect their own
> accounts, and restore their usable environment without becoming a platform
> engineer.

This is stronger than a new Proxmox skin, but much narrower than a new private
cloud platform. It must beat configured existing tools on recurring operator
work, not on the number of features in a screenshot.

This assessment combines public documentation with the existing
[local substrate spike](proxmox-spike.md). Competitors were not installed or
benchmarked for this assessment. No additional host operations were performed.
Documented features are not claims about product polish, deployment quality,
security assurance, or commercial traction.

## Direct competitive overlap

| Documented alternative | What overlaps | Implication |
| --- | --- | --- |
| Coder plus its community Proxmox VM template [1] | Proxmox-hosted development workspaces; the example uses Terraform, cloud-init and the Coder agent. | This combination already exists. Homecloud must beat its configured workflow, not claim the architectural combination is new. |
| Coder Agents and resource controls [2] | Self-hosted agent execution with workspace provisioning, and credit-based workspace quotas. | Agents and resource budgets are not an open category. Credits must not be conflated with host-enforced storage accounting. |
| PVE Panel [3] | Proxmox self-service, configurable resource quotas, templates and backup/restore workflows. | A cleaner VM portal with quotas and templates is not an unoccupied product position. |
| MultiPortal [4] | Multi-tenant Proxmox self-service and quota-driven resource allocation. | Giving coworkers delegated infrastructure is also an existing product category. |
| pve-agents [5] | Disposable Proxmox LXC environments for coding-agent jobs. Its security documentation describes single-operator use. | Proxmox-native agent orchestration is not unique; disposable jobs are a different emphasis from persistent personal workstations. |

The reviewed sources do not establish an exact turnkey T3 onboarding,
host-bounded storage, and coworker recovery product. That is a limit of this
research, not proof that none exists or that these alternatives cannot be
configured to provide it.

Recovery coordination is not unoccupied either: PVE Panel documents checking
restores against a customer's plan, re-isolating the restored server, and
restricting backup visibility to the current ownership period [3]. A local
Homecloud recovery finding is not evidence that these competitors ignore
ownership.

Coder's Community edition is free, while its documentation labels its standard
credit-based resource quotas Premium [6]. Running third-party coding tools in
workspaces is also distinct from using Coder's own agent product [2]. Do not
pitch Homecloud simply as the
cheap alternative to an inevitably expensive Coder deployment. Actual costs,
licensing needs and setup effort depend on the configured baseline.

Coder also publishes a NixOS workspace example using flakes [7]. Nix can improve
Homecloud's supported recipes; using it is not by itself exclusive
differentiation.

## The proposed customer, not yet a proven market

Start with a deliberately narrow recruiting hypothesis: a two-to-ten-person
development team that already owns or controls a Proxmox server, actively wants
remote T3 environments, and has no dedicated platform engineer.

The owner wants to share equipment safely, not learn another infrastructure
framework. Coworkers want a personal environment that is ready to use and
independent of their laptop connection. This is a proposed customer profile,
not a finding from interviews.

An installed Proxmox base could lower the adoption barrier: no host reinstall
and no requirement to replace the owner's trusted hypervisor UI. But existing
Proxmox operators may be sufficiently happy with their scripts and templates.
That is a central risk, not a minor marketing issue.

Avoid initially targeting general-purpose hosting providers, enterprise
workspace-platform buyers, or everyone interested in homelabs. Those are
different support and purchasing situations.

## What the product would actually promise

### 1. A supported personal-workstation workflow

An owner invites a coworker and grants resources. The coworker provisions a
supported T3 recipe, authorizes their own accounts, and reaches a usable backend
without handling Proxmox credentials or infrastructure templates.

Show application readiness separately from VM power state: installed version,
T3 service health, connection/setup state and an actionable repair path. Keep
the actual coding interaction in T3 instead of building a second agent UI.

Persistence must be stated precisely. Laptop-disconnect independence, VM reboot
recovery, host reboot recovery and resuming an interrupted model task are
different promises. The spike did not test authenticated T3 disconnects, real
model tasks, host reboot or model-task resumption.

### 2. A truthful, enforced resource contract

The owner's grant must mean more than a portal allocation counter. The
existing spike demonstrated practical host-enforced ZFS tenant boundaries,
including snapshot accounting, with a privileged provisioning helper. It did
not demonstrate an API-only implementation or a complete hostile-tenant
security boundary.

Track granted budget, virtual allocation, backing reservations, snapshots,
guest-reported use and pool headroom separately. Budget backups, image caches
and host/global storage separately too. Host-backed quotas are a useful
service invariant, not a new ZFS invention or evidence that competing portals
lack them. Shared I/O contention and physical-host failure remain possible.

Use shared vCPUs and conservative RAM admission initially. Exclusive core
distribution need not be carried forward to preserve the customer promise.

### 3. Recovery restores the coworker's access, not only their disk

Treat owner identity, storage placement, access policy and T3 readiness as part
of a restore. In the spike, deleting and restoring a VM preserved its data and
T3 service but required reapplying its VM-scoped ACL. This demonstrates real
integration work; it does not prove competitors handle it badly.

Likewise, offboarding must identify and deal with every supported access path.
The current managed-app documentation explicitly notes that disabling a
Homecloud account does not automatically revoke T3 Connect authorization. Do
not advertise one-click complete revocation until that boundary has a defined,
tested workflow.

### 4. A narrow operational surface

Let Proxmox remain responsible for hypervisor operations and expose its native
owner UI for advanced administration and emergencies. Homecloud should provide
team ownership, supported guest recipes, meaningful status and coordinated
recovery. A read-only physical-health summary may help owners; rebuilding
every ZFS administration operation is not necessary to validate this wedge.

Homecloud still needs desired-state and authorization reconciliation. Wrapping
Proxmox removes hypervisor implementation work, not all control-plane work.

## Why this could still fail

- A configured Coder deployment or a self-service portal with a prepared T3
  image might be good enough. Competitors need not match every detail to win.
- Plain Proxmox templates and a small onboarding script might be the stronger
  substitute for this customer than a full workspace platform.
- T3 support may be an excellent entry point but too small a market, or a
  workflow that T3 or another remote-development product could absorb.
- The pain might be initial setup rather than recurring operations. That
  supports an installation service more readily than a recurring software
  business.
- Keeping a physical team server reliable creates responsibilities software
  alone cannot remove: capacity, networking, backups, power and hardware repair.
- Host-backed limits, recipes and a clean interface are reproducible by other
  teams. Any longer-term advantage would have to be earned through adoption,
  trusted operations, compatibility work, support and distribution.

These are hypotheses and strategic risks, not measured customer findings.

## The next experiment

Build a small vertical slice on a disposable Proxmox installation, not a
replacement for the running Homecloud host. Recruit three unrelated teams
matching the proposed profile. Ask about recent actual onboarding, access,
capacity and recovery incidents before showing the product.

Use the same hardware and prepared images to compare:

1. Homecloud's proposed workflow.
2. Coder configured with the community Proxmox template.
3. A reasonable simple baseline: Proxmox templates plus documented onboarding;
   optionally a self-service portal if it matches the team's requirements.

Give the baselines competent initial configuration. Penalizing Coder because
the evaluator has not configured its template would not establish a wedge.
Count setup work separately from recurring operator work.

Run these tasks:

- Invite a coworker, grant resources and reach a real authorized T3 session.
- Start actual work, disconnect the laptop, and verify the specified persistence
  behavior rather than substituting a service-health check.
- Attempt to exceed a tenant's storage budget while another tenant writes;
  include snapshots, cloning and restore/admission paths in the eventual suite.
- Restore an environment and verify data, tenant placement, access and service
  readiness. Do not imply interrupted model-task resumption unless tested.
- Remove a coworker and verify the defined revocation boundary, including
  independent T3 access and saved credentials.

Record successful completion, owner interventions, privileged/manual commands,
time to a usable environment, and recurring maintenance effort. Aesthetic
preference is secondary. Observe pilots for several weeks and ask for actual
payment for the defined outcome.

Continue if independent teams pay and demonstrably need less recurring expert
help than with the best-fit configured baseline. Narrow to a setup/support
offering if value ends after installation. Stop claiming a software wedge if
customers are happy with existing tools or the benefit is predominantly a
prettier screen.

## Sources

1. [Coder community Proxmox VM template](https://registry.coder.com/templates/umair/proxmox-vm),
   [README](https://github.com/coder/registry/blob/main/registry/umair/templates/proxmox-vm/README.md),
   [Terraform implementation](https://github.com/coder/registry/blob/main/registry/umair/templates/proxmox-vm/main.tf).
2. [Coder Agents](https://coder.com/docs/ai-coder/agents),
   [resource quotas](https://coder.com/docs/admin/users/quotas).
3. [PVE Panel repository and documented features](https://github.com/sebastianflint/pve-panel).
4. [MultiPortal overview](https://multiportal.io/).
5. [pve-agents repository](https://github.com/ndom91/pve-agents),
   [single-operator scope and security model](https://github.com/ndom91/pve-agents/blob/main/docs/security-and-networking.md).
6. [Coder pricing and feature tiers](https://coder.com/pricing),
   [quota feature tier](https://coder.com/docs/admin/users/quotas).
7. [Coder NixOS example flake](https://github.com/coder/nixos-example-flake).
8. [Local Proxmox experiment and limitations](proxmox-spike.md#results),
   [current managed-application authorization boundary](managed-apps.md#authorization-and-recovery).
