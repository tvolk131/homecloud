use super::*;

impl Store {
    pub fn bind_virtualization_backend(&self, identity: &str) -> Result<()> {
        self.transaction(|transaction| {
            let previous: Option<String> = transaction.query_row("SELECT value FROM meta WHERE key='virtualization_backend'", [], |row| row.get(0)).optional()?;
            match previous {
                Some(previous) => ensure!(previous == identity, "This state directory belongs to a different virtualization backend; use a separate state directory"),
                None => { transaction.execute("INSERT INTO meta VALUES ('virtualization_backend',?)", [identity])?; }
            }
            Ok(())
        })
    }
    pub fn virtualization_operation(&self, id: Uuid) -> Result<Option<VirtualizationOperation>> {
        self.connect()?
            .query_row(
                "SELECT record FROM virtualization_operations WHERE id=?",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .map(parse)
            .transpose()
    }

    pub fn virtualization_operations(&self) -> Result<Vec<VirtualizationOperation>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT record FROM virtualization_operations ORDER BY active DESC, rowid DESC LIMIT 100",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|record| parse(record?))
            .collect()
    }

    pub fn reserve_virtualization_operation(
        &self,
        operation: VirtualizationOperation,
    ) -> Result<(VirtualizationOperation, bool)> {
        operation.guest.validate()?;
        self.transaction(|transaction| {
            if let Some(record) = transaction.query_row(
                "SELECT record FROM virtualization_operations WHERE id=?", [operation.id.to_string()], |row| row.get(0),
            ).optional()? {
                let existing: VirtualizationOperation = parse(record)?;
                ensure!(existing.requested_by == operation.requested_by && existing.guest == operation.guest && existing.action == operation.action,
                    "Power request key was already used for a different request");
                return Ok((existing, false));
            }
            let count: u32 = transaction.query_row("SELECT COUNT(*) FROM virtualization_operations WHERE active=1", [], |row| row.get(0))?;
            ensure!(count < 32, "Too many active power operations");
            let occupied: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM virtualization_operations WHERE node=? AND vmid=? AND active=1)",
                params![operation.guest.node, operation.guest.vmid], |row| row.get(0),
            )?;
            ensure!(!occupied, "This guest already has an active or uncertain power operation");
            ensure!(operation.phase == VirtualizationOperationPhase::Submitting && operation.task_id.is_none(), "Invalid initial power operation");
            transaction.execute("INSERT INTO virtualization_operations VALUES (?,?,?,?,?)",
                params![operation.id.to_string(), operation.guest.node, operation.guest.vmid, true, json(&operation)?])?;
            Ok((operation, true))
        })
    }

    pub fn update_virtualization_operation(
        &self,
        operation: &VirtualizationOperation,
    ) -> Result<()> {
        self.transaction(|transaction| {
            let record = transaction.query_row(
                "SELECT record FROM virtualization_operations WHERE id=?",
                [operation.id.to_string()],
                |row| row.get(0),
            )?;
            let existing: VirtualizationOperation = parse(record)?;
            ensure!(
                existing.guest == operation.guest
                    && existing.action == operation.action
                    && existing.requested_by == operation.requested_by,
                "Power operation identity changed"
            );
            let valid = match existing.phase {
                VirtualizationOperationPhase::Submitting => matches!(
                    operation.phase,
                    VirtualizationOperationPhase::Running
                        | VirtualizationOperationPhase::Failed
                        | VirtualizationOperationPhase::Unknown
                ),
                VirtualizationOperationPhase::Running => matches!(
                    operation.phase,
                    VirtualizationOperationPhase::Running
                        | VirtualizationOperationPhase::Succeeded
                        | VirtualizationOperationPhase::Failed
                ),
                VirtualizationOperationPhase::Unknown => {
                    operation.phase == VirtualizationOperationPhase::Acknowledged
                }
                _ => false,
            };
            ensure!(valid, "Invalid power operation transition");
            ensure!(
                existing.phase == VirtualizationOperationPhase::Submitting
                    || existing.task_id == operation.task_id,
                "Power task identity changed"
            );
            ensure!(
                operation.phase != VirtualizationOperationPhase::Running
                    || operation
                        .task_id
                        .as_ref()
                        .is_some_and(|task| !task.is_empty()),
                "A running power operation requires a task identity"
            );
            transaction.execute(
                "UPDATE virtualization_operations SET active=?,record=? WHERE id=?",
                params![
                    operation.phase.blocks_guest(),
                    json(operation)?,
                    operation.id.to_string()
                ],
            )?;
            Ok(())
        })
    }

    pub fn interrupt_virtualization_submissions(&self) -> Result<()> {
        for mut operation in self.virtualization_operations()? {
            if operation.phase == VirtualizationOperationPhase::Submitting {
                operation.phase = VirtualizationOperationPhase::Unknown;
                operation.error = Some("Controller restarted during submission. The action may have reached Proxmox. Inspect Proxmox before releasing this safety hold; it will not be retried automatically.".into());
                operation.updated_at = now();
                self.update_virtualization_operation(&operation)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(vmid: u32) -> VirtualizationOperation {
        VirtualizationOperation {
            id: Uuid::new_v4(),
            requested_by: "owner".into(),
            guest: VirtualizationGuestIdentity {
                node: "pve".into(),
                vmid,
                kind: VirtualizationGuestKind::VirtualMachine,
            },
            action: VirtualizationPowerAction::Start,
            phase: VirtualizationOperationPhase::Submitting,
            task_id: None,
            error: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn open(directory: &tempfile::TempDir) -> Store {
        Store::open(
            directory.path().join("virtualization.db"),
            Limits {
                capacity: Resources::default(),
                core_groups: vec![],
                disk_overhead_gib_per_vm: 2,
            },
        )
        .unwrap()
    }

    #[test]
    fn backend_binding_rejects_switching_source_or_endpoint_in_place() {
        let directory = tempfile::tempdir().unwrap();
        let store = open(&directory);
        store.bind_virtualization_backend("simulated").unwrap();
        drop(store);
        let store = open(&directory);
        store.bind_virtualization_backend("simulated").unwrap();
        assert!(
            store
                .bind_virtualization_backend("proxmox:https://pve:8006")
                .is_err()
        );
    }

    #[test]
    fn reservations_are_atomic_and_keys_cannot_change_actor_or_payload() {
        let directory = tempfile::tempdir().unwrap();
        let store = open(&directory);
        let original = operation(101);
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let original = original.clone();
                std::thread::spawn(move || {
                    store.reserve_virtualization_operation(original).unwrap().1
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| usize::from(worker.join().unwrap()))
                .sum::<usize>(),
            1
        );
        for changed in [
            VirtualizationOperation {
                requested_by: "other".into(),
                ..original.clone()
            },
            VirtualizationOperation {
                action: VirtualizationPowerAction::Shutdown,
                ..original.clone()
            },
            VirtualizationOperation {
                guest: VirtualizationGuestIdentity {
                    vmid: 102,
                    ..original.guest.clone()
                },
                ..original.clone()
            },
        ] {
            assert!(store.reserve_virtualization_operation(changed).is_err());
        }
        assert!(
            store
                .reserve_virtualization_operation(operation(101))
                .is_err()
        );
    }

    #[test]
    fn task_transitions_cannot_report_success_without_a_task_or_replace_it() {
        let directory = tempfile::tempdir().unwrap();
        let store = open(&directory);
        let mut operation = operation(101);
        store
            .reserve_virtualization_operation(operation.clone())
            .unwrap();
        operation.phase = VirtualizationOperationPhase::Succeeded;
        assert!(store.update_virtualization_operation(&operation).is_err());
        operation.phase = VirtualizationOperationPhase::Running;
        assert!(store.update_virtualization_operation(&operation).is_err());
        operation.task_id = Some("first-task".into());
        store.update_virtualization_operation(&operation).unwrap();
        operation.task_id = Some("other-task".into());
        assert!(store.update_virtualization_operation(&operation).is_err());
        operation.task_id = Some("first-task".into());
        operation.phase = VirtualizationOperationPhase::Succeeded;
        store.update_virtualization_operation(&operation).unwrap();
        operation.phase = VirtualizationOperationPhase::Running;
        assert!(store.update_virtualization_operation(&operation).is_err());
    }

    #[test]
    fn active_operations_remain_visible_even_after_history_limit() {
        let directory = tempfile::tempdir().unwrap();
        let store = open(&directory);
        let pending = operation(101);
        store
            .reserve_virtualization_operation(pending.clone())
            .unwrap();
        let oldest_key = Uuid::new_v4();
        let oldest = VirtualizationOperation {
            id: oldest_key,
            ..operation(102)
        };
        for mut operation in std::iter::once(oldest.clone()).chain((103..210).map(operation)) {
            store
                .reserve_virtualization_operation(operation.clone())
                .unwrap();
            operation.phase = VirtualizationOperationPhase::Failed;
            store.update_virtualization_operation(&operation).unwrap();
        }
        let listed = store.virtualization_operations().unwrap();
        assert_eq!(listed.len(), 100);
        assert_eq!(listed[0].id, pending.id);
        assert!(!listed.iter().any(|operation| operation.id == oldest_key));
        assert!(!store.reserve_virtualization_operation(oldest).unwrap().1);
        store.interrupt_virtualization_submissions().unwrap();
        assert_eq!(
            store
                .virtualization_operation(pending.id)
                .unwrap()
                .unwrap()
                .phase,
            VirtualizationOperationPhase::Unknown
        );
    }
}
