use super::*;

pub enum CreateOutcome {
    Accepted(Operation),
    Rejected(String),
}
impl Store {
    /// A committed rejection is also an idempotent outcome: a delayed copy can
    /// never create a VM after the client has been told it may amend the draft.
    pub fn submit_create(&self, peer: &str, key: Uuid, spec: VmSpec) -> Result<CreateOutcome> {
        let request = Mutation::Create { spec };
        self.transaction(|tx| {
            let me=actor(tx,peer)?;
            let payload=json(&request)?;
            if let Some(record)=tx.query_row("SELECT record FROM operations WHERE actor=? AND request_key=?",
                params![me.id.to_string(),key.to_string()],|r| r.get::<_,String>(0)).optional()? {
                let op:Operation=parse(record)?;
                ensure!(json(&op.request)?==payload,"Idempotency key reused for a different request");
                return Ok(CreateOutcome::Accepted(op));
            }
            if let Some((old,message))=tx.query_row("SELECT request_json,message FROM create_rejections WHERE actor=? AND request_key=?",
                params![me.id.to_string(),key.to_string()],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()? {
                ensure!(old==payload,"Idempotency key reused for a different request");
                return Ok(CreateOutcome::Rejected(message));
            }
            tx.execute_batch("SAVEPOINT create_admission")?;
            match self.submit_in_transaction(tx,peer,key,request.clone()) {
                Ok(op)=>{
                    tx.execute_batch("RELEASE create_admission")?;
                    Ok(CreateOutcome::Accepted(op))
                }
                Err(error)=>{
                    // Roll back partial network/core/app claims before saving
                    // the negative receipt in the same outer transaction.
                    tx.execute_batch("ROLLBACK TO create_admission; RELEASE create_admission")?;
                    let message=error.to_string();
                    tx.execute("INSERT INTO create_rejections VALUES (?,?,?,?)",
                        params![me.id.to_string(),key.to_string(),payload,message])?;
                    audit(tx,peer,"create_rejected",&key.to_string())?;
                    Ok(CreateOutcome::Rejected(message))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store, User) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path().join("state.db"),
            Limits {
                capacity: Resources {
                    memory_bytes: 8 * GIB,
                    cpu_cores: 2,
                    disk_bytes: 128 * GIB,
                },
                core_groups: vec![vec![2, 8], vec![3, 9]],
                disk_overhead_gib_per_vm: 2,
            },
        )
        .unwrap();
        let (user, token) = store.bootstrap("Owner").unwrap();
        store.enroll(&token, "owner").unwrap();
        (dir, store, user)
    }
    fn spec() -> VmSpec {
        VmSpec {
            name: "recoverable".into(),
            memory_gib: 2,
            cores: 1,
            disk_gib: 10,
            applications: vec![],
            ssh_public_key:
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                    .into(),
        }
    }
    fn accepted(result: CreateOutcome) -> Operation {
        match result {
            CreateOutcome::Accepted(op) => op,
            CreateOutcome::Rejected(message) => panic!("{message}"),
        }
    }
    fn claims(store: &Store, table: &str) -> i64 {
        store
            .connect()
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    #[test]
    fn accepted_create_replay_survives_full_quota_restart_and_concurrent_requests() {
        let (dir, store, user) = fixture();
        let key = Uuid::new_v4();
        let grant = spec().resources(2).unwrap();
        store.grant("owner", user.id, grant, false).unwrap();
        let first = accepted(store.submit_create("owner", key, spec()).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let mut tasks = vec![];
        for _ in 0..8 {
            let other = Store::open(dir.path().join("state.db"), store.limits().clone()).unwrap();
            let b = barrier.clone();
            tasks.push(std::thread::spawn(move || {
                b.wait();
                accepted(other.submit_create("owner", key, spec()).unwrap())
            }));
        }
        barrier.wait();
        for task in tasks {
            let replay = task.join().unwrap();
            assert_eq!(replay.id, first.id);
            assert_eq!(replay.vm_id, first.vm_id);
        }
        assert_eq!(store.vms().unwrap().len(), 1);
        assert_eq!(claims(&store, "cpu_leases"), 2);
        assert_eq!(claims(&store, "network_leases"), 1);
        let owner = store
            .users()
            .unwrap()
            .into_iter()
            .find(|u| u.id == user.id)
            .unwrap();
        assert_eq!(owner.committed, grant);
    }
    #[test]
    fn committed_rejection_fences_delayed_and_legacy_copies_after_capacity_changes() {
        let (dir, store, user) = fixture();
        let key = Uuid::new_v4();
        assert!(matches!(
            store.submit_create("owner", key, spec()).unwrap(),
            CreateOutcome::Rejected(_)
        ));
        let store = Store::open(dir.path().join("state.db"), store.limits().clone()).unwrap();
        store
            .grant("owner", user.id, store.limits().capacity, false)
            .unwrap();
        assert!(matches!(
            store.submit_create("owner", key, spec()).unwrap(),
            CreateOutcome::Rejected(_)
        ));
        assert!(
            store
                .submit("owner", key, Mutation::Create { spec: spec() })
                .is_err()
        );
        let mut changed = spec();
        changed.name = "amended".into();
        assert!(store.submit_create("owner", key, changed).is_err());
        assert!(store.vms().unwrap().is_empty());
        assert_eq!(claims(&store, "cpu_leases"), 0);
        assert_eq!(claims(&store, "network_leases"), 0);
        accepted(
            store
                .submit_create("owner", Uuid::new_v4(), spec())
                .unwrap(),
        );
        assert_eq!(store.vms().unwrap().len(), 1);
    }
    #[test]
    fn rejected_partial_creation_rolls_back_all_claims_before_saving_receipt() {
        let (_dir, store, user) = fixture();
        store
            .grant("owner", user.id, store.limits().capacity, false)
            .unwrap();
        store.connect().unwrap().execute_batch("CREATE TRIGGER fail_app BEFORE INSERT ON applications BEGIN SELECT RAISE(ABORT,'injected app failure'); END").unwrap();
        let mut desired = spec();
        desired.applications = vec![ApplicationKind::T3];
        assert!(matches!(
            store
                .submit_create("owner", Uuid::new_v4(), desired)
                .unwrap(),
            CreateOutcome::Rejected(_)
        ));
        for table in [
            "vms",
            "cpu_leases",
            "network_leases",
            "operations",
            "applications",
            "application_operations",
        ] {
            assert_eq!(claims(&store, table), 0, "{table}");
        }
        assert_eq!(claims(&store, "create_rejections"), 1);
    }
    #[test]
    fn unauthenticated_failures_are_not_creation_rejections() {
        let (_dir, store, _) = fixture();
        assert!(
            store
                .submit_create("stranger", Uuid::new_v4(), spec())
                .is_err()
        );
        assert_eq!(claims(&store, "create_rejections"), 0);
    }
}
