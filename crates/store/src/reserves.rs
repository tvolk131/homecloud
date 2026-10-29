use super::*;

pub(super) fn active(conn: &Connection) -> Result<HostReserves> {
    conn.query_row(
        "SELECT value FROM meta WHERE key='host_reserves'",
        [],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(parse)
    .transpose()
    .map(|v| v.unwrap_or_default())
}
pub(super) fn pending(conn: &Connection) -> Result<Option<HostReserveOperation>> {
    conn.query_row(
        "SELECT record FROM host_reserve_operations WHERE phase NOT IN ('succeeded','cancelled')",
        [],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(parse)
    .transpose()
}
fn operation(conn: &Connection, id: Uuid) -> Result<HostReserveOperation> {
    parse(conn.query_row(
        "SELECT record FROM host_reserve_operations WHERE id=?",
        [id.to_string()],
        |r| r.get(0),
    )?)
}
fn protected(conn: &Connection) -> Result<Resources> {
    users(conn)?
        .into_iter()
        .try_fold(Resources::default(), |sum, u| {
            sum.checked_add(u.grant.max(u.committed))
                .map_err(anyhow::Error::msg)
        })
}
fn save_operation(tx: &Transaction<'_>, op: &HostReserveOperation) -> Result<()> {
    let phase = serde_json::to_value(op.phase)?;
    tx.execute(
        "UPDATE host_reserve_operations SET phase=?,record=? WHERE id=?",
        params![phase.as_str().unwrap(), json(op)?, op.id.to_string()],
    )?;
    Ok(())
}

impl Store {
    /// Conservative throughout a host operation: reduced capacity is fenced
    /// immediately; released capacity waits for verified host completion.
    pub(super) fn admission_limits(&self, conn: &Connection) -> Result<Limits> {
        let current = active(conn)?;
        let next = pending(conn)?;
        let reserve = next
            .as_ref()
            .map(|op| current.additional.max(op.target.additional))
            .unwrap_or(current.additional);
        let mut limits = self.limits.clone();
        if let Some(memory) = self.memory_status_from(conn)? {
            limits.capacity.memory_bytes = memory.reviewed_bytes.min(memory.safe_bytes);
        }
        if let Some(op) = memory::pending(conn)? {
            limits.capacity.memory_bytes = limits.capacity.memory_bytes.min(op.target_bytes);
        }
        let memory = limits
            .capacity
            .memory_bytes
            .saturating_sub(reserve.memory_bytes);
        limits.capacity = Resources {
            memory_bytes: 0,
            ..limits.capacity
        }
        .checked_sub(Resources {
            memory_bytes: 0,
            ..reserve
        })
        .map_err(anyhow::Error::msg)?;
        limits.capacity.memory_bytes = memory;
        limits.core_groups.retain(|group| {
            !current
                .core_groups
                .iter()
                .chain(next.iter().flat_map(|op| op.target.core_groups.iter()))
                .any(|held| held == group)
        });
        ensure!(
            limits.core_groups.len() as u64 == limits.capacity.cpu_cores,
            "Reserve CPU fence does not match capacity"
        );
        Ok(limits)
    }
    pub fn capacity(&self) -> Result<Resources> {
        let conn = self.connect()?;
        let tx = conn.unchecked_transaction()?;
        Ok(self.admission_limits(&tx)?.capacity)
    }
    pub fn require_host_reserve_policy(&self) -> Result<()> {
        self.connect()?.execute(
            "INSERT OR IGNORE INTO meta VALUES ('host_reserve_policy_required','1')",
            [],
        )?;
        Ok(())
    }
    pub fn requires_host_reserve_policy(&self) -> Result<bool> {
        Ok(self.has_host_reserve_history()?
            || self.connect()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM meta WHERE key='host_reserve_policy_required')",
                [],
                |r| r.get::<_, bool>(0),
            )?)
    }
    pub fn host_reserve_state(&self) -> Result<HostReserves> {
        active(&self.connect()?)
    }
    pub fn host_reserve_operation(&self) -> Result<Option<HostReserveOperation>> {
        pending(&self.connect()?)
    }
    pub fn has_host_reserve_history(&self) -> Result<bool> {
        let conn = self.connect()?;
        Ok(active(&conn)?.revision > 0 || pending(&conn)?.is_some())
    }
    fn check_reserve_policy(&self, conn: &Connection, policy: &HostReservePolicy) -> Result<()> {
        ensure!(
            policy.envelope
                == Resources {
                    memory_bytes: policy
                        .memory
                        .as_ref()
                        .map(|m| m.reviewed_bytes)
                        .unwrap_or(self.limits.capacity.memory_bytes),
                    ..self.limits.capacity
                }
                && policy.core_groups == self.limits.core_groups
                && policy.disk_overhead_gib_per_vm == self.limits.disk_overhead_gib_per_vm,
            "Host reserve safety envelope differs from the controller configuration"
        );
        let current = active(conn)?;
        if let Some(op) = pending(conn)? {
            ensure!(op.before == current, "Reserve history is inconsistent");
            ensure!(
                (policy.state == op.before && !policy.applying)
                    || (policy.state == op.target && policy.operation_id == Some(op.id)),
                "Host reserve policy differs from the database; allocation is paused for review"
            );
        } else {
            ensure!(
                policy.state == current,
                "Host reserve policy differs from the database; allocation is paused for review"
            );
        }
        Ok(())
    }
    pub fn verify_host_reserve_policy(&self, policy: &HostReservePolicy) -> Result<()> {
        self.transaction(|tx| {
            let exclusions=&policy.excluded_network_slots;
            let mut unique=exclusions.clone();unique.sort();unique.dedup();
            ensure!(unique.len()==exclusions.len() && unique.iter().all(|s|(1..=200).contains(s)),"Invalid network slot exclusions");
            let old:Option<String>=tx.query_row("SELECT value FROM meta WHERE key='network_exclusions'",[],|r|r.get(0)).optional()?;
            if let Some(old)=old {ensure!(parse::<Vec<u32>>(old)?==*exclusions,"Host network exclusions changed; review existing bridge placement before allocating");}
            else {tx.execute("INSERT INTO meta VALUES ('network_exclusions',?)",[json(exclusions)?])?;}
            if let Some(memory) = &policy.memory {
                self.record_memory_status(tx, memory)?;
            }
            self.check_reserve_policy(tx, policy)
        })
    }
    pub fn host_reserve_view(&self, policy: &HostReservePolicy) -> Result<HostReserveView> {
        let conn = self.connect()?;
        let tx = conn.unchecked_transaction()?;
        self.check_reserve_policy(&tx, policy)?;
        let operation: Option<HostReserveOperation> = tx
            .query_row(
                "SELECT record FROM host_reserve_operations ORDER BY rowid DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(parse)
            .transpose()?;
        let can_discard = operation.as_ref().is_some_and(|op| {
            op.phase == ReservePhase::NeedsReview && policy.state == op.before && !policy.applying
        });
        Ok(HostReserveView {
            active: active(&tx)?,
            operation,
            envelope: policy.envelope,
            memory: policy.memory.clone(),
            memory_operation: self.pending_memory_capacity()?,
            protected_backing: protected(&tx)?,
            base_pool_free_bytes: policy.base_pool_free_bytes,
            can_discard,
            minimum_reserves: policy.minimum_reserves,
            arc: policy.arc.clone(),
            memory_policy: policy.memory_policy,
        })
    }
    pub fn set_host_reserves(
        &self,
        peer: &str,
        key: Uuid,
        expected_revision: u64,
        additional: Resources,
    ) -> Result<HostReserveOperation> {
        self.set_host_policy(peer, key, expected_revision, additional, None, None)
    }
    pub fn set_host_policy(
        &self,
        peer: &str,
        key: Uuid,
        expected_revision: u64,
        additional: Resources,
        arc: Option<ArcMode>,
        memory_policy: Option<HostMemoryPolicy>,
    ) -> Result<HostReserveOperation> {
        if let Some(mode) = &arc {
            mode.validate().map_err(anyhow::Error::msg)?;
        }
        self.transaction(|tx| {
            let me = admin(tx, peer)?;
            if let Some(record) = tx
                .query_row(
                    "SELECT record FROM host_reserve_operations WHERE actor=? AND request_key=?",
                    params![me.id.to_string(), key.to_string()],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            {
                let op: HostReserveOperation = parse(record)?;
                ensure!(
                    op.before.revision == expected_revision
                        && op.target.additional == additional
                        && op.target.arc == arc.clone().or(op.before.arc.clone()),
                    "Idempotency key reused for a different reserve change"
                );
                return Ok(op);
            }
            ensure!(
                pending(tx)?.is_none(),
                "Finish the current host reserve change first"
            );
            let before = active(tx)?;
            ensure!(
                before.revision == expected_revision,
                "Host reserves changed; refresh before saving"
            );
            ensure!(
                additional.memory_bytes.is_multiple_of(GIB)
                    && additional.disk_bytes.is_multiple_of(GIB),
                "Use whole GiB for host reserves"
            );
            let effective_arc = arc.as_ref().or(before.arc.as_ref());
            if let Some(mode) = effective_arc {
                let policy = memory_policy
                    .context("Host ARC budget is unavailable; review the host profile")?;
                let total = policy
                    .minimum_system_bytes
                    .checked_add(additional.memory_bytes)
                    .context("System memory overflow")?;
                policy.maximum(total, mode).map_err(anyhow::Error::msg)?;
            }
            let mut envelope = self.limits.capacity;
            if let Some(memory) = self.memory_status_from(tx)? {
                envelope.memory_bytes = memory.reviewed_bytes.min(memory.safe_bytes);
            }
            let capacity = envelope
                .checked_sub(additional)
                .map_err(anyhow::Error::msg)?;
            let old_capacity = self.admission_limits(tx)?.capacity;
            let promises = protected(tx)?;
            Resources {
                memory_bytes: if capacity.memory_bytes >= old_capacity.memory_bytes {
                    promises.memory_bytes
                } else {
                    0
                },
                cpu_cores: if capacity.cpu_cores >= old_capacity.cpu_cores {
                    promises.cpu_cores
                } else {
                    0
                },
                disk_bytes: if capacity.disk_bytes >= old_capacity.disk_bytes {
                    promises.disk_bytes
                } else {
                    0
                },
            }
            .admit(promises, capacity)
            .map_err(|e| {
                anyhow::anyhow!("Cannot reserve capacity already promised to users: {e}")
            })?;
            let mut groups = before.core_groups.clone();
            groups.truncate(additional.cpu_cores as usize);
            for group in self.limits.core_groups.iter().rev() {
                if groups.len() as u64 == additional.cpu_cores {
                    break;
                }
                if groups.contains(group) {
                    continue;
                }
                let free = group.iter().try_fold(true, |free, cpu| -> Result<bool> {
                    Ok(free
                        && !tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM cpu_leases WHERE cpu_id=?)",
                            [cpu],
                            |r| r.get::<_, bool>(0),
                        )?)
                })?;
                if free {
                    groups.push(group.clone());
                }
            }
            ensure!(
                groups.len() as u64 == additional.cpu_cores,
                "Not enough unclaimed complete physical cores for this reserve"
            );
            let target = HostReserves {
                revision: before
                    .revision
                    .checked_add(1)
                    .context("Reserve revision overflow")?,
                additional,
                core_groups: groups,
                arc: arc.or(before.arc.clone()),
            };
            let op = HostReserveOperation {
                id: Uuid::new_v4(),
                actor: me.id,
                request_key: key,
                before,
                target,
                phase: ReservePhase::Pending,
                error: None,
            };
            tx.execute(
                "INSERT INTO host_reserve_operations VALUES (?,?,?,'pending',?)",
                params![
                    op.id.to_string(),
                    me.id.to_string(),
                    key.to_string(),
                    json(&op)?
                ],
            )?;
            audit(tx, peer, "set_host_reserves", &op.id.to_string())?;
            Ok(op)
        })
    }
    pub fn prepare_host_reserves(&self) -> Result<Option<HostReserveOperation>> {
        self.transaction(|tx| {
            let Some(mut op) = pending(tx)? else {
                return Ok(None);
            };
            if op.phase == ReservePhase::NeedsReview {
                return Ok(None);
            }
            ensure!(op.before == active(tx)?, "Reserve history is inconsistent");
            op.phase = ReservePhase::Applying;
            op.error = None;
            save_operation(tx, &op)?;
            Ok(Some(op))
        })
    }
    pub fn wait_host_reserves(&self, id: Uuid, error: String) -> Result<()> {
        self.transaction(|tx| {
            let mut op = operation(tx, id)?;
            ensure!(!op.phase.terminal(), "Reserve operation already finished");
            op.phase = ReservePhase::Applying;
            op.error = Some(error);
            save_operation(tx, &op)
        })
    }
    pub fn fail_host_reserves(&self, id: Uuid, error: String) -> Result<()> {
        self.transaction(|tx| {
            let mut op = operation(tx, id)?;
            ensure!(
                !op.phase.terminal(),
                "Host reserve operation already finished"
            );
            op.phase = ReservePhase::NeedsReview;
            op.error = Some(error);
            save_operation(tx, &op)
        })
    }
    pub fn finish_host_reserves(&self, id: Uuid, policy: &HostReservePolicy) -> Result<()> {
        self.transaction(|tx| {
            let mut op=operation(tx,id)?;
            ensure!(policy.state==op.target && !policy.applying && policy.memory.as_ref().is_none_or(|m|m.arc_ready) && policy.operation_id==Some(id),"Host has not confirmed reserve enforcement");
            if op.phase==ReservePhase::Succeeded {return Ok(())}
            ensure!(!op.phase.terminal() && active(tx)?==op.before,"Reserve completion is stale");
            self.check_reserve_policy(tx,policy)?;
            tx.execute("INSERT INTO meta VALUES ('host_reserves',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[json(&op.target)?])?;
            op.phase=ReservePhase::Succeeded;op.error=None;save_operation(tx,&op)?;
            audit(tx,&op.actor.to_string(),"host_reserves_applied",&id.to_string())
        })
    }
    pub fn retry_host_reserves(&self, peer: &str, id: Uuid) -> Result<HostReserveOperation> {
        self.transaction(|tx| {
            admin(tx, peer)?;
            let mut op = operation(tx, id)?;
            ensure!(
                !op.phase.terminal(),
                "Host reserve operation already finished"
            );
            op.phase = ReservePhase::Pending;
            op.error = None;
            save_operation(tx, &op)?;
            audit(tx, peer, "retry_host_reserves", &id.to_string())?;
            Ok(op)
        })
    }
    pub fn prepare_reserve_discard(&self, peer: &str, id: Uuid) -> Result<HostReserveOperation> {
        self.transaction(|tx| {
            admin(tx, peer)?;
            let op = operation(tx, id)?;
            ensure!(
                op.phase == ReservePhase::NeedsReview,
                "Only a reserve change needing review can be discarded"
            );
            audit(tx, peer, "request_discard_host_reserves", &id.to_string())?;
            Ok(op)
        })
    }
    /// Caller also holds the controller's reconciliation/admission locks while
    /// checking this host attestation, so no in-flight apply can follow discard.
    pub fn discard_host_reserves(
        &self,
        peer: &str,
        id: Uuid,
        policy: &HostReservePolicy,
    ) -> Result<()> {
        self.transaction(|tx| {
            admin(tx, peer)?;
            let mut op = operation(tx, id)?;
            if op.phase == ReservePhase::Cancelled {
                return Ok(());
            }
            ensure!(
                op.phase == ReservePhase::NeedsReview
                    && policy.state == op.before
                    && !policy.applying,
                "Only a confirmed unapplied reserve change can be discarded; retry recovery instead"
            );
            self.check_reserve_policy(tx, policy)?;
            op.phase = ReservePhase::Cancelled;
            save_operation(tx, &op)?;
            audit(tx, peer, "discard_host_reserves", &id.to_string())
        })
    }
}
