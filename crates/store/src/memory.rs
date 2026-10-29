use super::*;

pub(super) fn pending(conn: &Connection) -> Result<Option<MemoryCapacityOperation>> {
    conn.query_row(
        "SELECT record FROM memory_capacity_operations WHERE terminal=0",
        [],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(parse)
    .transpose()
}

pub(super) fn totals(conn: &Connection) -> Result<(Resources, Resources)> {
    users(conn)?
        .into_iter()
        .try_fold((Resources::default(), Resources::default()), |(p, h), u| {
            Ok((
                p.checked_add(u.grant.max(u.committed))
                    .map_err(anyhow::Error::msg)?,
                h.checked_add(u.committed).map_err(anyhow::Error::msg)?,
            ))
        })
}
impl Store {
    pub fn memory_health(&self, peer: &str, verified: bool) -> Result<Option<HostMemoryHealth>> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let me = actor(&tx, peer)?;
        let Some(status) = self.memory_status_from(&tx)? else {
            return Ok(if verified {
                None
            } else {
                Some(HostMemoryHealth {
                    condition: MemoryCondition::Unavailable,
                    details: None,
                })
            });
        };
        let (promised, held) = totals(&tx)?;
        let capacity = self.admission_limits(&tx)?.capacity.memory_bytes;
        let condition = if !verified || now().saturating_sub(status.observed_at) > 30 {
            MemoryCondition::Unavailable
        } else if !status.arc_ready {
            MemoryCondition::ArcNotEnforced
        } else if held.memory_bytes > capacity {
            MemoryCondition::AllocationsExceedCapacity
        } else if promised.memory_bytes > capacity {
            MemoryCondition::PromisesExceedCapacity
        } else {
            MemoryCondition::Healthy
        };
        Ok(Some(HostMemoryHealth {
            condition,
            details: (me.role == Role::Admin).then_some(MemoryHealthDetails {
                capacity_bytes: capacity,
                promised_bytes: promised.memory_bytes,
                held_bytes: held.memory_bytes,
            }),
        }))
    }

    pub(super) fn memory_status_from(
        &self,
        conn: &Connection,
    ) -> Result<Option<MemoryCapacityStatus>> {
        conn.query_row(
            "SELECT value FROM meta WHERE key='memory_status'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(parse)
        .transpose()
    }
    pub(super) fn record_memory_status(
        &self,
        tx: &Transaction<'_>,
        status: &MemoryCapacityStatus,
    ) -> Result<()> {
        Uuid::parse_str(&status.boot_id)?;
        ensure!(
            status.safe_bytes
                <= status
                    .physical_bytes
                    .saturating_sub(status.minimum_system_bytes)
                && status.safe_bytes.is_multiple_of(GIB),
            "Invalid physical memory attestation"
        );
        ensure!(
            status.observed_at <= now() + 5 && now().saturating_sub(status.observed_at) <= 30,
            "Memory observation is stale"
        );
        if let Some(old) = self.memory_status_from(tx)? {
            ensure!(
                status.revision >= old.revision,
                "Memory capacity journal moved backwards; inspect restoration before allocating"
            );
            ensure!(
                status.revision != old.revision || status.reviewed_bytes == old.reviewed_bytes,
                "Memory capacity changed without a reviewed revision"
            );
        }
        tx.execute("INSERT INTO meta VALUES ('memory_status',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[json(status)?])?;
        Ok(())
    }
    pub fn memory_capacity_request(
        &self,
        peer: &str,
        id: Uuid,
        expected_revision: u64,
        target_bytes: u64,
    ) -> Result<MemoryCapacityOperation> {
        self.transaction(|tx| {
            let me = admin(tx, peer)?;
            if let Some(record) = tx
                .query_row(
                    "SELECT record FROM memory_capacity_operations WHERE id=?",
                    [id.to_string()],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            {
                let old: MemoryCapacityOperation = parse(record)?;
                ensure!(
                    old.actor == me.id
                        && old.expected_revision == expected_revision
                        && old.target_bytes == target_bytes,
                    "Memory request key reused"
                );
                ensure!(
                    !old.cancelled,
                    "This memory capacity request was cancelled; use a new request key"
                );
                return Ok(old);
            }
            ensure!(
                reserves::pending(tx)?.is_none(),
                "Finish the host reserve change before adopting capacity"
            );
            let status = self
                .memory_status_from(tx)?
                .context("Physical memory observation is unavailable")?;
            ensure!(
                status.revision == expected_revision
                    && target_bytes <= status.safe_bytes
                    && target_bytes.is_multiple_of(GIB),
                "Memory capacity changed or target is unsafe; refresh before adopting"
            );
            ensure!(
                status.arc_ready,
                "Repair ARC enforcement before adopting memory capacity"
            );
            ensure!(
                target_bytes >= reserves::active(tx)?.additional.memory_bytes,
                "Lower additional memory reserves before adopting this smaller envelope"
            );
            ensure!(
                !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM memory_capacity_operations WHERE terminal=0)",
                    [],
                    |r| r.get::<_, bool>(0)
                )?,
                "Finish the pending memory capacity change first"
            );
            let op = MemoryCapacityOperation {
                id,
                actor: me.id,
                expected_revision,
                target_bytes,
                succeeded: false,
                cancelled: false,
            };
            tx.execute(
                "INSERT INTO memory_capacity_operations VALUES (?,?,0,?)",
                params![id.to_string(), me.id.to_string(), json(&op)?],
            )?;
            audit(tx, peer, "review_memory_capacity", &id.to_string())?;
            Ok(op)
        })
    }
    pub fn cancel_memory_capacity(
        &self,
        peer: &str,
        id: Uuid,
        proof: &MemoryCapacityStatus,
    ) -> Result<()> {
        self.transaction(|tx| {
            admin(tx, peer)?;
            let mut op: MemoryCapacityOperation = parse(tx.query_row(
                "SELECT record FROM memory_capacity_operations WHERE id=?",
                [id.to_string()],
                |r| r.get(0),
            )?)?;
            ensure!(
                !op.succeeded && proof.revision == op.expected_revision,
                "Capacity was applied or changed; finish recovery instead of cancelling"
            );
            self.record_memory_status(tx, proof)?;
            op.cancelled = true;
            tx.execute(
                "UPDATE memory_capacity_operations SET terminal=1,record=? WHERE id=?",
                params![json(&op)?, id.to_string()],
            )?;
            audit(
                tx,
                peer,
                "cancel_unapplied_memory_capacity",
                &id.to_string(),
            )
        })
    }
    pub fn pending_memory_capacity(&self) -> Result<Option<MemoryCapacityOperation>> {
        pending(&self.connect()?)
    }
    pub fn finish_memory_capacity(&self, id: Uuid, proof: &MemoryCapacityStatus) -> Result<()> {
        self.transaction(|tx| {
            let mut op: MemoryCapacityOperation = parse(tx.query_row(
                "SELECT record FROM memory_capacity_operations WHERE id=?",
                [id.to_string()],
                |r| r.get(0),
            )?)?;
            ensure!(
                !op.cancelled,
                "A cancelled memory capacity request cannot complete"
            );
            ensure!(
                proof.revision == op.expected_revision + 1
                    && proof.reviewed_bytes == op.target_bytes,
                "Host did not confirm the requested memory capacity"
            );
            self.record_memory_status(tx, proof)?;
            op.succeeded = true;
            tx.execute(
                "UPDATE memory_capacity_operations SET terminal=1,record=? WHERE id=?",
                params![json(&op)?, id.to_string()],
            )?;
            audit(
                tx,
                &op.actor.to_string(),
                "memory_capacity_applied",
                &id.to_string(),
            )
        })
    }
    /// Once per kernel boot, reassert only already-held Running intent. A daemon
    /// restart or transient Incus Stopped window cannot create another launch.
    pub fn resume_host_boot(&self) -> Result<()> {
        self.transaction(|tx| {
            let Some(status)=self.memory_status_from(tx)? else { return Ok(()) };
            if !status.arc_ready || now().saturating_sub(status.observed_at)>30 {return Ok(())}
            let seen:Option<String>=tx.query_row("SELECT value FROM meta WHERE key='resumed_host_boot'",[],|r|r.get(0)).optional()?;
            if seen.as_deref()==Some(&status.boot_id) {return Ok(())}
            let (_,held)=totals(tx)?;
            if held.memory_bytes>self.admission_limits(tx)?.capacity.memory_bytes {return Ok(())}
            let records={let mut s=tx.prepare("SELECT record FROM vms")?;s.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?};
            for record in records {
                let vm:Vm=parse(record)?;
                if vm.desired!=Lifecycle::Running || !vm.has_compute() || vm.observed=="deleted" {continue}
                if tx.query_row("SELECT EXISTS(SELECT 1 FROM operations WHERE vm_id=? AND phase NOT IN ('succeeded','failed'))",[vm.id.to_string()],|r|r.get::<_,bool>(0))? {continue}
                let op=Operation{id:Uuid::new_v4(),vm_id:vm.id,actor:vm.owner,request:Mutation::State{vm_id:vm.id,desired:Lifecycle::Running},phase:"pending".into(),error:None,progress:None};
                let key=format!("boot:{}:{}",status.boot_id,vm.id);
                tx.execute("INSERT INTO operations VALUES (?,?,?,?,?,?)",params![op.id.to_string(),op.actor.to_string(),key,vm.id.to_string(),op.phase,json(&op)?])?;
                audit(tx,"controller","resume_after_host_boot",&vm.id.to_string())?;
            }
            tx.execute("INSERT INTO meta VALUES ('resumed_host_boot',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[status.boot_id])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store, User) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(
            dir.path().join("state.db"),
            Limits {
                capacity: Resources {
                    memory_bytes: 32 * GIB,
                    cpu_cores: 2,
                    disk_bytes: 128 * GIB,
                },
                core_groups: vec![vec![2, 8], vec![3, 9]],
                disk_overhead_gib_per_vm: 2,
            },
        )
        .unwrap();
        let (u, t) = s.bootstrap("Admin").unwrap();
        s.enroll(&t, "admin").unwrap();
        s.grant(
            "admin",
            u.id,
            Resources {
                memory_bytes: 24 * GIB,
                cpu_cores: 2,
                disk_bytes: 128 * GIB,
            },
            false,
        )
        .unwrap();
        (dir, s, u)
    }
    fn proof(safe: u64, reviewed: u64) -> MemoryCapacityStatus {
        MemoryCapacityStatus {
            revision: 0,
            reviewed_bytes: reviewed * GIB,
            safe_bytes: safe * GIB,
            physical_bytes: (safe + 4) * GIB,
            minimum_system_bytes: 4 * GIB,
            boot_id: Uuid::nil().to_string(),
            observed_at: now(),
            arc_ready: true,
            error: None,
        }
    }
    fn attest(s: &Store, p: &MemoryCapacityStatus) {
        s.transaction(|tx| s.record_memory_status(tx, p)).unwrap();
    }
    fn spec(memory: u64) -> VmSpec {
        VmSpec {
            applications: vec![],
            name: "Memory proof".into(),
            memory_gib: memory,
            cores: 1,
            disk_gib: 10,
            ssh_public_key:
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                    .into(),
        }
    }
    fn create(s: &Store, memory: u64) -> Operation {
        let o = s
            .submit(
                "admin",
                Uuid::new_v4(),
                Mutation::Create { spec: spec(memory) },
            )
            .unwrap();
        s.complete(o.id, "running".into(), None, false).unwrap();
        o
    }

    #[test]
    fn growth_stays_unissued_until_reviewed_and_survives_reopening() {
        let (dir, s, _) = fixture();
        let p = proof(60, 32);
        attest(&s, &p);
        assert_eq!(s.capacity().unwrap().memory_bytes, 32 * GIB);
        let id = Uuid::new_v4();
        let op = s.memory_capacity_request("admin", id, 0, 60 * GIB).unwrap();
        assert!(!op.succeeded);
        let applied = MemoryCapacityStatus {
            revision: 1,
            reviewed_bytes: 60 * GIB,
            ..p
        };
        s.finish_memory_capacity(id, &applied).unwrap();
        let reopened = Store::open(dir.path().join("state.db"), s.limits().clone()).unwrap();
        assert_eq!(reopened.capacity().unwrap().memory_bytes, 60 * GIB);
        assert!(
            reopened
                .memory_capacity_request("admin", id, 0, 60 * GIB)
                .unwrap()
                .succeeded
        );
        assert!(
            reopened
                .memory_capacity_request("admin", id, 0, 59 * GIB)
                .is_err()
        );
        assert_eq!(s.users().unwrap()[0].grant.memory_bytes, 24 * GIB);
    }
    #[test]
    fn shrink_keeps_claims_and_allows_recovery_before_new_allocation() {
        let (_dir, s, u) = fixture();
        let vm = create(&s, 16);
        attest(&s, &proof(12, 32));
        assert_eq!(s.capacity().unwrap().memory_bytes, 12 * GIB);
        assert_eq!(s.vm(vm.vm_id).unwrap().resources.memory_bytes, 16 * GIB);
        let mut grant = s.users().unwrap()[0].grant;
        grant.memory_bytes = 20 * GIB;
        s.grant("admin", u.id, grant, false).unwrap();
        assert!(
            s.submit("admin", Uuid::new_v4(), Mutation::Create { spec: spec(1) })
                .is_err()
        );
        let stop = s
            .submit(
                "admin",
                Uuid::new_v4(),
                Mutation::State {
                    vm_id: vm.vm_id,
                    desired: Lifecycle::Stopped,
                },
            )
            .unwrap();
        assert!(s.complete(stop.id, "stopped".into(), None, false).is_err());
        assert_eq!(s.vm(vm.vm_id).unwrap().resources.memory_bytes, 16 * GIB);
        s.complete(stop.id, "stopped".into(), None, true).unwrap();
        assert_eq!(s.vm(vm.vm_id).unwrap().resources.memory_bytes, 0);
        let edit = s
            .submit(
                "admin",
                Uuid::new_v4(),
                Mutation::Resize {
                    vm_id: vm.vm_id,
                    memory_gib: 8,
                    cores: 1,
                    disk_gib: 10,
                },
            )
            .unwrap();
        s.complete(edit.id, "stopped".into(), None, true).unwrap();
        assert!(
            s.submit(
                "admin",
                Uuid::new_v4(),
                Mutation::Start {
                    vm_id: vm.vm_id,
                    without_gpu: false
                }
            )
            .is_err()
        );
        grant.memory_bytes = 12 * GIB;
        s.grant("admin", u.id, grant, false).unwrap();
        s.submit(
            "admin",
            Uuid::new_v4(),
            Mutation::Start {
                vm_id: vm.vm_id,
                without_gpu: false,
            },
        )
        .unwrap();
        assert_eq!(s.users().unwrap()[0].committed.memory_bytes, 8 * GIB);
    }
    #[test]
    fn boot_recovery_is_once_per_kernel_boot_and_never_releases_claims() {
        let (_dir, s, _) = fixture();
        let vm = create(&s, 16);
        let mut p = proof(12, 32);
        attest(&s, &p);
        s.resume_host_boot().unwrap();
        assert!(s.pending().unwrap().is_empty());
        p.safe_bytes = 32 * GIB;
        p.physical_bytes = 36 * GIB;
        attest(&s, &p);
        s.resume_host_boot().unwrap();
        let pending = s.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].vm_id, vm.vm_id);
        s.complete(pending[0].id, "running".into(), None, false)
            .unwrap();
        s.resume_host_boot().unwrap();
        assert!(s.pending().unwrap().is_empty());
        p.boot_id = Uuid::new_v4().to_string();
        attest(&s, &p);
        s.resume_host_boot().unwrap();
        assert_eq!(s.pending().unwrap().len(), 1);
        assert_eq!(s.vm(vm.vm_id).unwrap().resources.memory_bytes, 16 * GIB);
    }
    #[test]
    fn stale_attestations_and_capacity_rollback_are_rejected() {
        let (_dir, s, _) = fixture();
        let mut p = proof(32, 32);
        p.observed_at = now() - 31;
        assert!(s.transaction(|tx| s.record_memory_status(tx, &p)).is_err());
        p.observed_at = now();
        p.revision = 2;
        attest(&s, &p);
        p.revision = 1;
        assert!(s.transaction(|tx| s.record_memory_status(tx, &p)).is_err());
    }
    #[test]
    fn managed_host_disk_is_backed_and_requires_admin_at_creation() {
        let (_dir, s, u) = fixture();
        let mut spec = spec(8);
        spec.cores = 2;
        spec.disk_gib = 32;
        spec.applications = vec![ApplicationKind::HomecloudHost];
        assert_eq!(spec.resources(2).unwrap().disk_bytes, 66 * GIB);
        let (token, _, _) = s
            .invite(
                "admin",
                "Member",
                Role::User,
                Resources::default(),
                false,
                1,
            )
            .unwrap();
        s.enroll(&token, "member").unwrap();
        assert!(
            s.submit(
                "member",
                Uuid::new_v4(),
                Mutation::Create { spec: spec.clone() }
            )
            .unwrap_err()
            .to_string()
            .contains("Administrator")
        );
        let op = s
            .submit("admin", Uuid::new_v4(), Mutation::Create { spec })
            .unwrap();
        assert_eq!(s.vm(op.vm_id).unwrap().resources.disk_bytes, 66 * GIB);
        assert_eq!(
            s.users()
                .unwrap()
                .iter()
                .find(|v| v.id == u.id)
                .unwrap()
                .committed
                .memory_bytes,
            8 * GIB
        );
    }
    #[test]
    fn unapplied_decrease_fences_admission_and_confirmed_cancel_keeps_a_receipt() {
        let (_dir, s, _) = fixture();
        let p = proof(32, 32);
        attest(&s, &p);
        let id = Uuid::new_v4();
        s.memory_capacity_request("admin", id, 0, 16 * GIB).unwrap();
        assert_eq!(s.capacity().unwrap().memory_bytes, 16 * GIB);
        s.cancel_memory_capacity("admin", id, &p).unwrap();
        assert!(s.pending_memory_capacity().unwrap().is_none());
        assert_eq!(s.capacity().unwrap().memory_bytes, 32 * GIB);
        assert!(s.memory_capacity_request("admin", id, 0, 16 * GIB).is_err());
        s.memory_capacity_request("admin", Uuid::new_v4(), 0, 32 * GIB)
            .unwrap();
    }
    #[test]
    fn initial_capacity_table_migrates_without_losing_an_unfinished_request() {
        let (dir, s, _) = fixture();
        attest(&s, &proof(32, 32));
        let id = Uuid::new_v4();
        s.memory_capacity_request("admin", id, 0, 16 * GIB).unwrap();
        s.connect()
            .unwrap()
            .execute_batch(
                "ALTER TABLE memory_capacity_operations RENAME COLUMN terminal TO succeeded;",
            )
            .unwrap();
        let reopened = Store::open(dir.path().join("state.db"), s.limits().clone()).unwrap();
        assert_eq!(reopened.pending_memory_capacity().unwrap().unwrap().id, id);
    }
}

#[cfg(test)]
mod multi_user_recovery {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store, User, User, User, [Uuid; 2]) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(
            dir.path().join("state.db"),
            Limits {
                capacity: Resources {
                    memory_bytes: 8 * GIB,
                    cpu_cores: 2,
                    disk_bytes: 48 * GIB,
                },
                core_groups: vec![vec![2, 8], vec![3, 9]],
                disk_overhead_gib_per_vm: 2,
            },
        )
        .unwrap();
        let (admin, token) = s.bootstrap("Admin").unwrap();
        s.enroll(&token, "admin").unwrap();
        let mut members = vec![];
        let mut ids = vec![];
        for peer in ["alice", "bob"] {
            let (token, _, _) = s
                .invite(
                    "admin",
                    peer,
                    Role::User,
                    Resources {
                        memory_bytes: 4 * GIB,
                        cpu_cores: 1,
                        disk_bytes: 24 * GIB,
                    },
                    false,
                    1,
                )
                .unwrap();
            let user = s.enroll(&token, peer).unwrap();
            members.push(user);
            let op=s.submit(peer,Uuid::new_v4(),Mutation::Create { spec:VmSpec {applications:vec![],name:peer.into(),memory_gib:2,cores:1,disk_gib:10,ssh_public_key:"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into()} }).unwrap();
            s.complete(op.id, "running".into(), None, false).unwrap();
            ids.push(op.vm_id);
        }
        (
            dir,
            s,
            admin,
            members[0].clone(),
            members[1].clone(),
            [ids[0], ids[1]],
        )
    }
    fn attest(s: &Store, safe: u64, ready: bool) {
        s.transaction(|tx| {
            s.record_memory_status(
                tx,
                &MemoryCapacityStatus {
                    revision: 0,
                    reviewed_bytes: 8 * GIB,
                    safe_bytes: safe * GIB,
                    physical_bytes: (safe + 4) * GIB,
                    minimum_system_bytes: 4 * GIB,
                    boot_id: Uuid::nil().to_string(),
                    observed_at: now(),
                    arc_ready: ready,
                    error: None,
                },
            )
        })
        .unwrap();
    }
    #[test]
    fn two_users_resume_only_when_all_held_memory_fits_and_warnings_survive_reopen() {
        let (dir, s, _, _, _, ids) = fixture();
        attest(&s, 3, true);
        s.resume_host_boot().unwrap();
        assert!(s.pending().unwrap().is_empty());
        let health = s.memory_health("admin", true).unwrap().unwrap();
        assert_eq!(health.condition, MemoryCondition::AllocationsExceedCapacity);
        assert_eq!(
            health.details.unwrap(),
            MemoryHealthDetails {
                capacity_bytes: 3 * GIB,
                promised_bytes: 8 * GIB,
                held_bytes: 4 * GIB
            }
        );
        let member = s.memory_health("alice", true).unwrap().unwrap();
        assert!(member.details.is_none());
        let reopened = Store::open(dir.path().join("state.db"), s.limits().clone()).unwrap();
        assert_eq!(
            reopened.memory_health("bob", true).unwrap().unwrap(),
            member
        );
        for id in ids {
            assert_eq!(s.vm(id).unwrap().resources.memory_bytes, 2 * GIB);
        }
        let stop = s
            .submit(
                "alice",
                Uuid::new_v4(),
                Mutation::State {
                    vm_id: ids[0],
                    desired: Lifecycle::Stopped,
                },
            )
            .unwrap();
        assert!(s.complete(stop.id, "stopped".into(), None, false).is_err());
        s.resume_host_boot().unwrap();
        assert_eq!(s.pending().unwrap().len(), 1);
        s.complete(stop.id, "stopped".into(), None, true).unwrap();
        s.resume_host_boot().unwrap();
        let pending = s.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].vm_id, ids[1]);
        s.complete(pending[0].id, "running".into(), None, false)
            .unwrap();
        s.resume_host_boot().unwrap();
        assert!(s.pending().unwrap().is_empty());
        assert_eq!(
            s.memory_health("admin", true).unwrap().unwrap().condition,
            MemoryCondition::PromisesExceedCapacity
        );
        assert!(
            s.submit(
                "alice",
                Uuid::new_v4(),
                Mutation::Start {
                    vm_id: ids[0],
                    without_gpu: false
                }
            )
            .is_err()
        );
    }
    #[test]
    fn excess_promises_allow_both_existing_claims_to_resume_but_arc_and_unknown_policy_block() {
        let (_dir, s, _, _, _, ids) = fixture();
        attest(&s, 5, true);
        s.resume_host_boot().unwrap();
        let pending = s.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert!(ids.iter().all(|id| pending.iter().any(|o| o.vm_id == *id)));
        assert_eq!(
            s.memory_health("admin", true).unwrap().unwrap().condition,
            MemoryCondition::PromisesExceedCapacity
        );
        attest(&s, 5, false);
        assert_eq!(
            s.memory_health("alice", true).unwrap().unwrap().condition,
            MemoryCondition::ArcNotEnforced
        );
        assert_eq!(
            s.memory_health("admin", false).unwrap().unwrap().condition,
            MemoryCondition::Unavailable
        );
    }
    #[test]
    fn starts_and_grant_increases_race_for_one_headroom_without_double_spending() {
        let (dir, s, _, alice, bob, ids) = fixture();
        for (peer, id) in [("alice", ids[0]), ("bob", ids[1])] {
            let stop = s
                .submit(
                    peer,
                    Uuid::new_v4(),
                    Mutation::State {
                        vm_id: id,
                        desired: Lifecycle::Stopped,
                    },
                )
                .unwrap();
            s.complete(stop.id, "stopped".into(), None, true).unwrap();
            let edit = s
                .submit(
                    peer,
                    Uuid::new_v4(),
                    Mutation::Resize {
                        vm_id: id,
                        memory_gib: 1,
                        cores: 1,
                        disk_gib: 10,
                    },
                )
                .unwrap();
            s.complete(edit.id, "stopped".into(), None, true).unwrap();
        }
        for u in [&alice, &bob] {
            s.grant(
                "admin",
                u.id,
                Resources {
                    memory_bytes: GIB,
                    cpu_cores: 1,
                    disk_bytes: 24 * GIB,
                },
                false,
            )
            .unwrap();
        }
        attest(&s, 3, true);
        let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
        let path = dir.path().join("state.db");
        let limits = s.limits().clone();
        let results = std::thread::scope(|scope| {
            let mut handles = vec![];
            for (peer, user, id) in [("alice", alice.id, ids[0]), ("bob", bob.id, ids[1])] {
                for grant in [false, true] {
                    let gate = gate.clone();
                    let path = path.clone();
                    let limits = limits.clone();
                    handles.push(scope.spawn(move || {
                        let db = Store::open(path, limits).unwrap();
                        gate.wait();
                        if grant {
                            (
                                true,
                                db.grant(
                                    "admin",
                                    user,
                                    Resources {
                                        memory_bytes: 2 * GIB,
                                        cpu_cores: 1,
                                        disk_bytes: 24 * GIB,
                                    },
                                    false,
                                )
                                .is_ok(),
                            )
                        } else {
                            (
                                false,
                                db.submit(
                                    peer,
                                    Uuid::new_v4(),
                                    Mutation::Start {
                                        vm_id: id,
                                        without_gpu: false,
                                    },
                                )
                                .is_ok(),
                            )
                        }
                    }));
                }
            }
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            results.iter().filter(|&&(grant, ok)| grant && ok).count(),
            1
        );
        assert_eq!(
            results.iter().filter(|&&(grant, ok)| !grant && ok).count(),
            2
        );
        let users = s.users().unwrap();
        assert_eq!(
            users.iter().map(|u| u.grant.memory_bytes).sum::<u64>(),
            3 * GIB
        );
        assert_eq!(
            users.iter().map(|u| u.committed.memory_bytes).sum::<u64>(),
            2 * GIB
        );
        let a = s.vm(ids[0]).unwrap();
        let b = s.vm(ids[1]).unwrap();
        assert!(!a.cpu_ids.iter().any(|id| b.cpu_ids.contains(id)));
        assert_eq!(s.pending().unwrap().len(), 2);
    }
}
