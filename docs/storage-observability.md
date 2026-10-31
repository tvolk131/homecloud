# Read-only storage observability

Administrators can open **Hosts → the host → Disks & ZFS** to inspect all
kernel-visible disks and imported ZFS pools. **Inspect /dev/…** expands a pool
leaf or disk's identity, reported SMART health and pool memberships.

This does not format, import, replace, offline, clear errors, repair, scrub,
enable SMART, or start a self-test. Only the private observation cache and lock
under `/var/lib/homecloud/host` are written. Alert history, notifications,
performance sampling, provisioning and maintenance controls remain separate work.

## Collection and freshness

- Collection, parsing, disk/vdev association and SMART caching are implemented in
  Rust in `homecloud-host`, compiled into the existing `homecloudd` binary. The
  existing Python privileged adapter only dispatches the fixed `collect-storage`
  command. This local-only command requires root on Linux, reads the fixed host
  policy and does not open the controller database or start a network listener.
- The controller collects at startup, then 60 seconds after each cycle completes.
  Desktop refreshes read cached storage data; slow probes do not hold its read lock.
- Discovery uses `lsblk --json --bytes --paths` with explicit columns. Topology
  uses `zpool status -j --json-int -P -p`, requiring OpenZFS 2.3+ JSON support.
  No locale-sensitive text parser or pool-import fallback is used.
- SMART uses `smartctl -j -x`, normally once per disk per 15 minutes. The
  versioned, root-owned `storage.json` cache preserves this interval across
  restarts. At most 32 disks are probed per cycle, oldest/unsampled first, with
  four workers and eight-second command timeouts. Discovery/ZFS also time out.
- Disks without serial/WWN identities remain probe-eligible each cycle. Only
  their last attempt timestamp carries over by path when other identity metadata
  matches, including through cache reloads and cycles when they are not sampled.
  This keeps scheduling fair without reusing health readings for an unidentified
  replacement. Changed model/size metadata discards the hint; future timestamps
  are treated as unsampled for ordering after a clock rollback.
- Colliding WWNs or model/serial identities use path-based IDs for every disk in
  the collision and follow the same scheduling-only cache policy. Neither device
  ordering nor removal of a colliding disk allows health readings to cross over.
  Older caches are also checked for ambiguous identities before reusing readings.
- Non-NVMe paths request `-n standby,99,98`. A low-power result is skipped without
  an active fallback. ATA devices also skip unsupported power checks via the
  second status argument; that argument is ATA-specific. NVMe omits this switch.
  A recent smartmontools supporting both status arguments is needed. Device
  auto-detection and bridges have hardware-specific behavior: this is not a
  guarantee that every controller stays asleep, and unsupported SCSI power checks
  may be ignored by smartctl. Missing tools/permissions are
  reported; installing smartmontools remains an operator action.
- Disk and topology sources have independent timestamps/errors. Failed/skipped
  probes retain previous readings without refreshing their successful sample
  timestamps. The native view displays sample age; exact epoch timestamps are
  included in admin CLI snapshot JSON. Disk/topology samples older than three
  minutes and SMART samples older than thirty minutes are not current.
- A reported SMART failure takes precedence over a probe/freshness warning in
  the disk badge. Retained failures are labelled `SMART reported failure (last
  sample)` when discovery or the health sample is not current, or probing was
  skipped/unavailable. A fresh failed health result remains prominent even when
  other SMART commands fail; those errors remain available in Inspect. A new
  passing observation can clear the retained failure warning.
- A subsequent read without an explicit health result cannot clear a retained
  failure, even if temperature or other metrics were returned. The previous
  observation and its original timestamp remain visible as last-sample data.
  Fresh passing/unknown health with partial-command errors instead uses an
  explicit `partial read` badge, not `not current` or an unqualified green pass.

Serials, topology and SMART details are omitted from non-admin snapshots; the UI
also checks the role. Older controllers show an upgrade/unavailable message.
Missing observations never imply that no disks/pools exist or that SMART passed.

## Identity and interpretation

WWN is preferred, then model/serial, then the current path. Only unique hardware
identities qualify for cached health reuse; the path fallback is not a stable
hardware identity. Partitions and resolved `/dev/disk/by-id` links
associate leaves with containing disks. Ambiguous/file-backed/missing leaves
remain unmatched; missing leaves retain their reported GUID. SMART serial
mismatches discard the reading. Disk rows can show OS/boot filesystem mounts.

Data, special, dedup, log, cache and spare roles are retained. Single-device
data/special/dedup vdevs receive a no-redundancy warning, not a complete
failure-tolerance analysis. ZFS errors and drive-reported SMART health remain
separate; ONLINE and SMART passed are not reliability guarantees.

ATA attributes retain raw vendor names/values rather than a universal health
score. NVMe includes warning flags, spare, percentage used, media errors and
usage counters when reported. Unsupported attributes/test logs are unreported,
not zero. smartctl health/log exit bits are not automatically probe failures.

Unmatched does not mean unused: a disk may belong to an exported pool or another
storage stack. Exported pools, enclosure/bay locations and drives hidden behind
hardware RAID are not discovered. NVMe namespaces can appear as separate block
disks with controller-wide health. Loop devices are not physical disks; file/loop
lab pools cannot prove physical SMART behavior.

The disposable-host rehearsal found that lsblk reports logical ZFS zvols such as
`/dev/zd0` as type `disk`. Discovery now requests the kernel device name (`KNAME`)
and excludes zvol names (`zd` followed by decimal digits) from disk rows and the
SMART probe budget. The device path is used when the kernel name is absent.
Reported ZFS vdev leaves still remain visible, but a logical zvol does not receive
a physical-disk match. No separate logical-volume inventory is added here.

Disk rows describe host-visible backing devices, not proof of direct access to
physical hardware. VM-exposed SCSI/virtio/NVMe disks remain discoverable, including
devices without model/serial/WWN metadata. Neither guest disk health nor a zvol's
unavailable SMART observation establishes the health of its underlying drives.

## Validation boundary

Deterministic tests cover topology, partition matching, missing/ambiguous leaves,
ATA/NVMe normalization, exit bits, sleeping/failed probes, caching, source
isolation, admin scoping and native inspection. Screenshots cover topology,
SMART details, small windows, and stale/unavailable observations.

Physical-host discovery, controller/USB behavior and actual SMART access still
need live acceptance. The pre-classification-fix bundle passed a native
deployment/recovery rehearsal in a disposable managed host, including real
privileged collection and an enrolled administrator snapshot before rollback; see
[validation](validation.md#disposable-native-deployment-rehearsal). That historical
run does not validate the corrected binary; refreshed native acceptance is still
required. The permanent controller was not upgraded. Deploying the new controller
and matched adapter there remains a separate operation. The collector
ships inside the existing daemon binary: no new installed helper, updater change,
sudo rule or service is required. Follow the normal matched-bundle software update
and rollback procedure in [deployment](deployment.md), which restarts only the
Homecloud daemon, not Incus or guests. Existing receipt, boundary, host-health and
version checks still apply; do not bypass them. Live upgrade/rollback rehearsal
and physical-hardware acceptance remain separate from deterministic tests.
