use super::*;

fn current(conn: &Connection) -> Result<Option<HostShutdownOperation>> {
    conn.query_row(
        "SELECT record FROM host_shutdown_operations ORDER BY rowid DESC LIMIT 1",
        [],
        |r| r.get(0),
    )
    .optional()?
    .map(parse)
    .transpose()
}
fn save(conn: &Connection, op: &mut HostShutdownOperation) -> Result<()> {
    op.revision += 1;
    conn.execute(
        "UPDATE host_shutdown_operations SET active=?,record=? WHERE id=?",
        params![op.phase.blocks_work(), json(op)?, op.id.to_string()],
    )?;
    Ok(())
}
fn review(conn: &Connection, peer: &str, id: Uuid, cancel: bool) -> Result<HostShutdownOperation> {
    admin(conn, peer)?;
    let op = current(conn)?.context("No host shutdown")?;
    ensure!(op.id == id, "Host shutdown changed; refresh its status");
    if (cancel && op.phase == HostShutdownPhase::Cancelled)
        || (!cancel
            && matches!(
                op.phase,
                HostShutdownPhase::Draining
                    | HostShutdownPhase::PoweroffPending
                    | HostShutdownPhase::PoweroffRequested
                    | HostShutdownPhase::Finished
            ))
    {
        return Ok(op);
    }
    ensure!(
        op.phase == HostShutdownPhase::NeedsReview,
        "Shutdown is not waiting for recovery"
    );
    ensure!(
        !cancel || !op.poweroff_attempted,
        "Power-off may already be queued; cancellation cannot keep this host on"
    );
    Ok(op)
}
fn request_admission(
    conn: &Connection,
    peer: &str,
    key: Uuid,
    boot_id: &str,
) -> Result<Option<HostShutdownOperation>> {
    let me = admin(conn, peer)?;
    if let Some(record) = conn
        .query_row(
            "SELECT record FROM host_shutdown_operations WHERE actor=? AND request_key=?",
            params![me.id.to_string(), key.to_string()],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        let op: HostShutdownOperation = parse(record)?;
        ensure!(
            op.boot_id == boot_id,
            "Shutdown request key belongs to another boot"
        );
        return Ok(Some(op));
    }
    ensure!(
        current(conn)?.is_none_or(|op| !op.phase.blocks_work()),
        "Host shutdown is already in progress"
    );
    for table in ["operations", "host_reserve_operations"] {
        let query = format!(
            "SELECT EXISTS(SELECT 1 FROM {table} WHERE phase NOT IN ('succeeded','failed','cancelled'))"
        );
        ensure!(
            !conn.query_row(&query, [], |r| r.get::<_, bool>(0))?,
            "Finish or recover pending VM and host operations before shutting down"
        );
    }
    ensure!(
        !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM memory_capacity_operations WHERE terminal=0)",
            [],
            |r| r.get::<_, bool>(0)
        )?,
        "Finish the memory capacity update before shutting down"
    );
    if let Some(vm) = vms(conn)?.into_iter().find(|vm| vm.observed == "missing") {
        anyhow::bail!(
            "Resolve the missing Incus instance for {} before shutting down this host. Recover or delete that VM first.",
            vm.spec.name
        );
    }
    Ok(None)
}
impl Store {
    /// Fast rejection before a shutdown writer can queue behind slow host work.
    /// The same checks are repeated transactionally after acquiring admission.
    pub fn review_shutdown_request(
        &self,
        peer: &str,
        key: Uuid,
        boot_id: &str,
    ) -> Result<Option<HostShutdownOperation>> {
        request_admission(&self.connect()?, peer, key, boot_id)
    }

    pub fn review_host_shutdown(
        &self,
        peer: &str,
        id: Uuid,
        cancel: bool,
    ) -> Result<HostShutdownOperation> {
        review(&self.connect()?, peer, id, cancel)
    }
    pub fn host_shutdown(&self) -> Result<Option<HostShutdownOperation>> {
        current(&self.connect()?)
    }

    pub fn request_host_shutdown(
        &self,
        peer: &str,
        key: Uuid,
        boot_id: String,
    ) -> Result<HostShutdownOperation> {
        self.transaction(|tx| {
            if let Some(op) = request_admission(tx, peer, key, &boot_id)? {
                return Ok(op);
            }
            let me = admin(tx, peer)?;
            let vm_ids = vms(tx)?
                .into_iter()
                .filter(|v| v.observed != "deleted")
                .map(|v| v.id)
                .collect::<Vec<_>>();
            let op = HostShutdownOperation {
                id: Uuid::new_v4(),
                actor: me.id,
                boot_id,
                revision: 0,
                phase: HostShutdownPhase::Draining,
                remaining: vm_ids.clone(),
                vm_ids,
                poweroff_attempted: false,
                poweroff_requested_at: None,
                error: None,
            };
            tx.execute(
                "INSERT INTO host_shutdown_operations VALUES (?,?,?,?,?)",
                params![
                    op.id.to_string(),
                    me.id.to_string(),
                    key.to_string(),
                    true,
                    json(&op)?
                ],
            )?;
            audit(tx, peer, "request_host_shutdown", &op.id.to_string())?;
            Ok(op)
        })
    }
    /// A restarted controller never resumes a destructive request without review.
    pub fn interrupt_host_shutdown(&self) -> Result<()> {
        self.transaction(|tx| {
            if let Some(mut op) = current(tx)?
                && matches!(op.phase, HostShutdownPhase::Draining | HostShutdownPhase::PoweroffPending | HostShutdownPhase::PoweroffRequested)
            {
                op.phase = HostShutdownPhase::NeedsReview;
                op.error = Some(if op.poweroff_attempted {
                    "The controller restarted after a power-off attempt. Its outcome is unknown. Review and retry if the host remains reachable."
                } else {
                    "The controller restarted during shutdown. Review and retry, or cancel shutdown and resume VMs."
                }.into());
                save(tx, &mut op)?;
                audit(tx,"controller","host_shutdown_needs_review",&op.id.to_string())?;
            }
            Ok(())
        })
    }
    pub fn sync_shutdown_boot(&self, boot_id: &str) -> Result<()> {
        self.transaction(|tx| {
            if let Some(mut op) = current(tx)?
                && op.phase.blocks_work()
                && op.boot_id != boot_id
            {
                op.phase = HostShutdownPhase::Finished;
                op.error = None;
                save(tx, &mut op)?;
                audit(
                    tx,
                    "controller",
                    "host_shutdown_boot_ended",
                    &op.id.to_string(),
                )?;
            }
            Ok(())
        })
    }
    pub fn update_host_shutdown(
        &self,
        mut op: HostShutdownOperation,
    ) -> Result<HostShutdownOperation> {
        self.transaction(|tx| {
            let old = current(tx)?.context("No host shutdown")?;
            ensure!(
                old.id == op.id && old.revision == op.revision,
                "Host shutdown changed; refresh its status"
            );
            if !old.poweroff_attempted && op.poweroff_attempted {
                audit(
                    tx,
                    "controller",
                    "host_poweroff_attempted",
                    &op.id.to_string(),
                )?;
            }
            if old.phase != op.phase && op.phase == HostShutdownPhase::NeedsReview {
                audit(
                    tx,
                    "controller",
                    "host_shutdown_needs_review",
                    &op.id.to_string(),
                )?;
            }
            save(tx, &mut op)?;
            Ok(op)
        })
    }
    pub fn recover_host_shutdown(
        &self,
        peer: &str,
        id: Uuid,
        cancel: bool,
    ) -> Result<HostShutdownOperation> {
        self.transaction(|tx| {
            let mut op = review(tx, peer, id, cancel)?;
            if op.phase != HostShutdownPhase::NeedsReview {
                return Ok(op);
            }
            op.error = None;
            if cancel {
                // This is an explicit resume, including a guest whose stop reply was lost.
                for id in &op.vm_ids {
                    let vm = get_vm(tx, *id)?;
                    if vm.desired != Lifecycle::Running
                        || !vm.has_compute()
                        || matches!(vm.observed.as_str(), "missing" | "unavailable")
                        || vm.error.as_deref().is_some_and(|e| e.contains("drift:"))
                    {
                        continue;
                    }
                    let resume = Operation {
                        id: Uuid::new_v4(),
                        actor: vm.owner,
                        vm_id: vm.id,
                        request: Mutation::State {
                            vm_id: vm.id,
                            desired: Lifecycle::Running,
                        },
                        phase: "pending".into(),
                        error: None,
                        progress: None,
                    };
                    tx.execute(
                        "INSERT INTO operations VALUES (?,?,?,?,?,?)",
                        params![
                            resume.id.to_string(),
                            resume.actor.to_string(),
                            format!("cancel-shutdown:{}:{}", op.id, vm.id),
                            vm.id.to_string(),
                            resume.phase,
                            json(&resume)?
                        ],
                    )?;
                }
                op.phase = HostShutdownPhase::Cancelled;
            } else {
                op.phase = HostShutdownPhase::Draining;
                op.remaining = op.vm_ids.clone();
            }
            save(tx, &mut op)?;
            audit(
                tx,
                peer,
                if cancel {
                    "cancel_host_shutdown_and_resume"
                } else {
                    "retry_host_shutdown"
                },
                &op.id.to_string(),
            )?;
            Ok(op)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controller_restart_audits_the_transition_to_review_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path().join("state.db"),
            Limits {
                capacity: Resources::default(),
                core_groups: vec![],
                disk_overhead_gib_per_vm: 2,
            },
        )
        .unwrap();
        let (_, token) = store.bootstrap("Admin").unwrap();
        store.enroll(&token, "admin").unwrap();
        let op = store
            .request_host_shutdown("admin", Uuid::new_v4(), Uuid::new_v4().to_string())
            .unwrap();
        store.interrupt_host_shutdown().unwrap();
        store.interrupt_host_shutdown().unwrap();
        let count:i64=store.connect().unwrap().query_row("SELECT count(*) FROM audit WHERE actor='controller' AND action='host_shutdown_needs_review' AND target=?",[op.id.to_string()],|r|r.get(0)).unwrap();
        assert_eq!(count, 1);
    }
}
