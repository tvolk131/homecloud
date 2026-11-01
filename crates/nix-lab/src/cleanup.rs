use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeletionStatus {
    Request,
    Pending,
    Complete,
}

fn deletion_status(state: &State, fixture: &Fixture, snapshot: &Value) -> Result<DeletionStatus> {
    let id = fixture.id.context("Fixture ID missing")?.to_string();
    let vm = snapshot["vms"]
        .as_array()
        .context("Snapshot VMs")?
        .iter()
        .find(|vm| vm["id"] == id);
    if let Some(vm) = vm {
        ensure!(
            vm["owner"] == state.owner.to_string()
                && vm["spec"]["name"] == fixture.name
                && vm["spec"]["applications"]
                    .as_array()
                    .is_none_or(Vec::is_empty),
            "Fixture identity or application changed"
        );
    }
    let operations: Vec<_> = snapshot["operations"]
        .as_array()
        .context("Snapshot operations")?
        .iter()
        .filter(|operation| {
            operation["vm_id"] == id
                && operation["actor"] == state.owner.to_string()
                && operation["request"]["kind"] == "state"
                && operation["request"]["vm_id"] == id
                && operation["request"]["desired"] == "deleted"
        })
        .collect();
    ensure!(operations.len() <= 1, "Ambiguous fixture deletion history");
    if let Some(operation) = operations.first() {
        ensure!(fixture.deletion_requested, "Unrecorded fixture deletion");
        return match operation["phase"].as_str() {
            Some("succeeded") if vm.is_none() => Ok(DeletionStatus::Complete),
            Some("pending" | "applying") if vm.is_some_and(|vm| vm["desired"] == "deleted") => {
                Ok(DeletionStatus::Pending)
            }
            _ => anyhow::bail!("Fixture deletion needs inspection: {operation}"),
        };
    }
    ensure!(
        vm.is_some_and(|vm| vm["desired"] != "deleted"),
        "Missing fixture or unconfirmed deletion; preserve resources for inspection"
    );
    Ok(DeletionStatus::Request)
}

impl Runner {
    fn cleanup_fence(&self, snapshot: &Value) -> Result<String> {
        let state = self.state.as_ref().context("Prepare fixtures first")?;
        state.check_policy(snapshot, state.restoring_grant)?;
        let mut script = String::from("set -eu\ninstances=$(incus list --format=json)\n");
        for fixture in &state.fixtures {
            let status = deletion_status(state, fixture, snapshot)?;
            let name = format!("hc-{}", fixture.id.context("Fixture ID missing")?.simple());
            script.push_str(&format!(
                "present=$(printf '%s' \"$instances\" | jq -er --arg name {} '[.[] | select(.name == $name)] | length')\n",
                quote(name)
            ));
            match status {
                DeletionStatus::Request => {
                    script.push_str("test \"$present\" -eq 1\n");
                    script.push_str(&fixture_fence(state, fixture)?);
                }
                DeletionStatus::Pending => {
                    script.push_str("test \"$present\" -le 1\nif test \"$present\" -eq 1; then\n");
                    script.push_str(&fixture_fence(state, fixture)?);
                    script.push_str("fi\n");
                }
                DeletionStatus::Complete => script.push_str("test \"$present\" -eq 0\n"),
            }
        }
        script.push_str(&self.protected_fence()?);
        script.push_str(&format!(
            "test ! -e /var/lib/homecloud/nix-lab/{}/active\n",
            state.run.simple()
        ));
        Ok(script)
    }

    pub(super) async fn cleanup(&mut self) -> Result<()> {
        let count = self
            .state
            .as_ref()
            .context("Prepare fixtures first")?
            .fixtures
            .len();
        for index in 0..count {
            let snapshot = self.snapshot().await?;
            self.remote(&self.cleanup_fence(&snapshot)?, 40)
                .await
                .context(
                    "Detach shared-store resources and verify surviving fixtures before cleanup",
                )?;
            let state = self.state.as_ref().unwrap();
            let fixture = &state.fixtures[index];
            if deletion_status(state, fixture, &snapshot)? != DeletionStatus::Request {
                continue;
            }
            let id = fixture.id.context("Fixture ID missing")?;
            self.state.as_mut().unwrap().fixtures[index].deletion_requested = true;
            self.save()?;
            self.cli(&["delete".into(), id.to_string()]).await?;
        }
        let start = Instant::now();
        loop {
            let snapshot = self.snapshot().await?;
            self.remote(&self.cleanup_fence(&snapshot)?, 40).await?;
            let state = self.state.as_ref().unwrap();
            let complete = state.fixtures.iter().try_fold(true, |complete, fixture| {
                Ok::<_, anyhow::Error>(
                    complete
                        && deletion_status(state, fixture, &snapshot)? == DeletionStatus::Complete,
                )
            })?;
            if complete {
                break;
            }
            ensure!(
                start.elapsed() < Duration::from_secs(600),
                "Deletion deadline; resume cleanup after inspection"
            );
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        if !self.state.as_ref().unwrap().restoring_grant {
            self.state
                .as_ref()
                .unwrap()
                .check_policy(&self.snapshot().await?, false)?;
            self.state.as_mut().unwrap().restoring_grant = true;
            self.save()?;
        }
        self.grant(&self.state.as_ref().unwrap().original_grant, true)
            .await?;
        self.state.as_mut().unwrap().cleaned = true;
        self.save()?;
        println!("Manifest-owned VMs deleted; original grant restored");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            version: 1,
            run: Uuid::new_v4(),
            owner: Uuid::new_v4(),
            original_grant: json!({"disk_bytes":64 * GIB}),
            applied_grant: json!({"disk_bytes":128 * GIB}),
            gpu_allowed: false,
            protected: json!([]),
            fixtures: vec![Fixture {
                role: "client-a".into(),
                name: "owned-client".into(),
                request: Uuid::new_v4(),
                id: Some(Uuid::new_v4()),
                memory_gib: 2,
                disk_gib: 12,
                deletion_requested: false,
            }],
            store_gib: 16,
            cleaned: false,
            restoring_grant: false,
        }
    }

    fn snapshot(state: &State) -> Value {
        json!({
            "me":{"id":state.owner,"role":"admin","grant":state.applied_grant,"gpu_allowed":state.gpu_allowed},
            "vms":[{"id":state.fixtures[0].id,"owner":state.owner,"desired":"running","spec":{"name":"owned-client","applications":[]}}],
            "operations":[]
        })
    }

    fn deletion(state: &State, phase: &str) -> Value {
        let id = state.fixtures[0].id.unwrap();
        json!({"id":Uuid::new_v4(),"vm_id":id,"actor":state.owner,"phase":phase,"request":{"kind":"state","vm_id":id,"desired":"deleted"}})
    }

    #[test]
    fn policy_fences_resources_gpu_and_administrator_identity() {
        let state = state();
        let mut current = snapshot(&state);
        assert!(state.check_policy(&current, false).is_ok());
        current["me"]["grant"] = state.original_grant.clone();
        assert!(state.check_policy(&current, true).is_ok());
        assert!(state.check_policy(&current, false).is_err());
        current["me"]["grant"] = json!({"disk_bytes":96 * GIB});
        assert!(state.check_policy(&current, true).is_err());
        current["me"]["grant"] = state.applied_grant.clone();
        current["me"]["gpu_allowed"] = json!(true);
        assert!(state.check_policy(&current, true).is_err());
        current["me"]["gpu_allowed"] = json!(false);
        current["me"]["id"] = json!(Uuid::new_v4());
        assert!(state.check_policy(&current, true).is_err());
    }

    #[test]
    fn cleanup_reconciles_a_lost_delete_reply_and_completed_deletion() {
        let mut state = state();
        let mut current = snapshot(&state);
        assert_eq!(
            deletion_status(&state, &state.fixtures[0], &current).unwrap(),
            DeletionStatus::Request
        );
        state.fixtures[0].deletion_requested = true;
        let state: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(
            deletion_status(&state, &state.fixtures[0], &current).unwrap(),
            DeletionStatus::Request
        );
        current["vms"][0]["desired"] = json!("deleted");
        current["operations"] = json!([deletion(&state, "applying")]);
        assert_eq!(
            deletion_status(&state, &state.fixtures[0], &current).unwrap(),
            DeletionStatus::Pending
        );
        current["vms"] = json!([]);
        current["operations"][0]["phase"] = json!("succeeded");
        assert_eq!(
            deletion_status(&state, &state.fixtures[0], &current).unwrap(),
            DeletionStatus::Complete
        );
    }

    #[test]
    fn cleanup_refuses_missing_unrecorded_failed_or_reassigned_fixtures() {
        let mut state = state();
        let mut current = snapshot(&state);
        current["vms"] = json!([]);
        assert!(deletion_status(&state, &state.fixtures[0], &current).is_err());
        current["operations"] = json!([deletion(&state, "succeeded")]);
        assert!(deletion_status(&state, &state.fixtures[0], &current).is_err());
        state.fixtures[0].deletion_requested = true;
        current = snapshot(&state);
        current["vms"][0]["desired"] = json!("deleted");
        for phase in ["failed", "needs_review"] {
            current["operations"] = json!([deletion(&state, phase)]);
            assert!(deletion_status(&state, &state.fixtures[0], &current).is_err());
        }
        current["operations"] = json!([deletion(&state, "applying")]);
        current["vms"][0]["owner"] = json!(Uuid::new_v4());
        assert!(deletion_status(&state, &state.fixtures[0], &current).is_err());
    }

    #[test]
    fn grant_restoration_recovery_accepts_only_captured_policy() {
        let mut state = state();
        state.restoring_grant = true;
        let state: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        let mut current = snapshot(&state);
        current["me"]["grant"] = state.original_grant.clone();
        assert!(state.check_policy(&current, state.restoring_grant).is_ok());
        current["me"]["gpu_allowed"] = json!(true);
        assert!(state.check_policy(&current, state.restoring_grant).is_err());
    }

    #[test]
    fn partial_cleanup_retains_fences_for_surviving_fixtures() {
        let mut state = state();
        let mut current = snapshot(&state);
        let surviving = state.fixtures[0].clone();
        state.fixtures[0].id = Some(Uuid::new_v4());
        state.fixtures[0].deletion_requested = true;
        current["operations"] = json!([deletion(&state, "succeeded")]);
        state.fixtures.push(surviving);
        assert_eq!(
            deletion_status(&state, &state.fixtures[0], &current).unwrap(),
            DeletionStatus::Complete
        );
        assert_eq!(
            deletion_status(&state, &state.fixtures[1], &current).unwrap(),
            DeletionStatus::Request
        );
        let runner = Runner {
            args: Args::try_parse_from([
                "lab",
                "--state",
                "unused",
                "--ssh-config",
                "unused",
                "cleanup",
            ])
            .unwrap(),
            profile: PathBuf::from("unused"),
            state: Some(state),
            _lock: fs::File::open(std::env::current_exe().unwrap()).unwrap(),
        };
        let script = runner.cleanup_fence(&current).unwrap();
        assert!(script.contains("test \"$present\" -eq 0"));
        assert!(script.contains("test \"$present\" -eq 1"));
        let state = runner.state.as_ref().unwrap();
        assert!(!script.contains(&format!(
            "incus config get 'hc-{}'",
            state.fixtures[0].id.unwrap().simple()
        )));
        assert!(script.contains(&fixture_fence(state, &state.fixtures[1]).unwrap()));
        current["vms"][0]["owner"] = json!(Uuid::new_v4());
        assert!(runner.cleanup_fence(&current).is_err());
    }
}
