use super::*;

fn application(conn: &Connection, id: Uuid) -> Result<ManagedApplication> {
    parse(conn.query_row(
        "SELECT record FROM applications WHERE id=?",
        [id.to_string()],
        |r| r.get(0),
    )?)
}
pub(super) fn insert_application(
    tx: &Transaction<'_>,
    vm_id: Uuid,
    kind: ApplicationKind,
) -> Result<ManagedApplication> {
    let kind_json = json(&kind)?;
    if let Some(record) = tx
        .query_row(
            "SELECT record FROM applications WHERE vm_id=? AND kind=?",
            params![vm_id.to_string(), kind_json],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return parse(record);
    }
    let app = ManagedApplication {
        id: Uuid::new_v4(),
        vm_id,
        kind,
        desired_version: kind.version().into(),
        start_on_boot: true,
        observation: ApplicationObservation::default(),
    };
    tx.execute(
        "INSERT INTO applications VALUES (?,?,?,?)",
        params![
            app.id.to_string(),
            vm_id.to_string(),
            kind_json,
            json(&app)?
        ],
    )?;
    Ok(app)
}
pub(super) fn enqueue(
    tx: &Transaction<'_>,
    app: &ManagedApplication,
    actor: Uuid,
    key: Uuid,
    kind: ApplicationOperationKind,
    setup_action: ApplicationSetupAction,
) -> Result<ApplicationOperation> {
    if let Some(record) = tx
        .query_row(
            "SELECT record FROM application_operations WHERE actor=? AND request_key=?",
            params![actor.to_string(), key.to_string()],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        let old: ApplicationOperation = parse(record)?;
        ensure!(
            old.application_id == app.id && old.kind == kind && old.setup_action == setup_action,
            "Idempotency key reused for a different application request"
        );
        return Ok(old);
    }
    if let Some(record) = tx.query_row("SELECT record FROM application_operations WHERE application_id=? AND phase IN ('pending','running','waiting_for_user')", [app.id.to_string()], |r| r.get::<_,String>(0)).optional()? {
        let old: ApplicationOperation = parse(record)?;
        ensure!(old.kind == kind && old.setup_action == setup_action && old.actor == actor, "Application already has an unfinished operation");
        return Ok(old);
    }
    let op = ApplicationOperation {
        id: Uuid::new_v4(),
        application_id: app.id,
        actor,
        kind,
        setup_action,
        phase: ApplicationPhase::Pending,
    };
    tx.execute(
        "INSERT INTO application_operations VALUES (?,?,?,?,?,?)",
        params![
            op.id.to_string(),
            app.id.to_string(),
            actor.to_string(),
            key.to_string(),
            op.phase.as_str(),
            json(&op)?
        ],
    )?;
    audit(
        tx,
        &actor.to_string(),
        "application_operation",
        &op.id.to_string(),
    )?;
    Ok(op)
}
impl Store {
    pub fn applications(&self) -> Result<Vec<ManagedApplication>> {
        let conn = self.connect()?;
        conn.prepare("SELECT record FROM applications ORDER BY rowid")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|row| parse(row?))
            .collect()
    }
    pub fn application_operations(&self) -> Result<Vec<ApplicationOperation>> {
        let conn = self.connect()?;
        conn.prepare("SELECT record FROM application_operations ORDER BY rowid")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|row| parse(row?))
            .collect()
    }
    pub fn ensure_application(
        &self,
        peer: &str,
        vm_id: Uuid,
        kind: ApplicationKind,
        key: Uuid,
    ) -> Result<ApplicationOperation> {
        self.transaction(|tx| {
            let me = actor(tx, peer)?;
            let vm = get_vm(tx, vm_id)?;
            ensure!(me.role == Role::Admin || me.id == vm.owner, "Forbidden");
            ensure!(
                vm.desired != Lifecycle::Deleted && vm.observed != "deleted",
                "VM deleted"
            );
            ensure!(
                !vm.spec.managed_host() || kind == ApplicationKind::HomecloudHost,
                "This VM is a managed Homecloud host"
            );
            // Host installation needs the separately backed disk created with
            // this workload. Never turn an ordinary user VM into a root host.
            ensure!(
                kind != ApplicationKind::HomecloudHost
                    || (me.role == Role::Admin && vm.spec.managed_host()),
                "Create an administrator-managed Homecloud host VM first"
            );
            let app = insert_application(tx, vm_id, kind)?;
            enqueue(
                tx,
                &app,
                me.id,
                key,
                ApplicationOperationKind::Install,
                ApplicationSetupAction::ConnectT3,
            )
        })
    }
    pub fn start_application_setup(
        &self,
        peer: &str,
        id: Uuid,
        key: Uuid,
        setup_action: ApplicationSetupAction,
    ) -> Result<ApplicationOperation> {
        self.transaction(|tx| {
            let me = actor(tx, peer)?;
            let app = application(tx, id)?;
            let vm = get_vm(tx, app.vm_id)?;
            ensure!(
                app.kind == ApplicationKind::T3,
                "This application has no account pairing step"
            );
            ensure!(me.role == Role::Admin || me.id == vm.owner, "Forbidden");
            if let Some(provider) = setup_action.provider_name() {
                ensure!(
                    me.id == vm.owner,
                    "Only the VM owner can sign into a provider account"
                );
                ensure!(
                    app.observation
                        .components
                        .iter()
                        .any(|c| c.name == provider && c.ready),
                    "Install the provider CLI first"
                );
            }
            ensure!(
                vm.desired == Lifecycle::Running && vm.observed == "running",
                "Start the VM before connecting the application"
            );
            ensure!(
                app.observation.installed_version.as_deref() == Some(&app.desired_version),
                "Install the application first"
            );
            enqueue(
                tx,
                &app,
                me.id,
                key,
                ApplicationOperationKind::Setup,
                setup_action,
            )
        })
    }
    /// Validate current owner/session/job before a transient code delivery.
    /// No code or challenge is accepted by, or persisted in, this layer.
    pub fn authorize_application_code(
        &self,
        peer: &str,
        id: Uuid,
        operation_id: Uuid,
    ) -> Result<(ManagedApplication, Vm, ApplicationOperation)> {
        self.transaction(|tx| {
            let me = actor(tx, peer)?;
            let app = application(tx, id)?;
            let vm = get_vm(tx, app.vm_id)?;
            ensure!(
                me.id == vm.owner,
                "Only the VM owner can submit a provider authorization code"
            );
            ensure!(
                vm.desired == Lifecycle::Running && vm.observed == "running",
                "Start the VM first"
            );
            let op: ApplicationOperation = parse(tx.query_row(
                "SELECT record FROM application_operations WHERE id=? AND application_id=?",
                params![operation_id.to_string(), id.to_string()],
                |r| r.get(0),
            )?)?;
            ensure!(
                op.kind == ApplicationOperationKind::Setup
                    && op.setup_action == ApplicationSetupAction::SignInClaude
                    && op.phase == ApplicationPhase::WaitingForUser,
                "This sign-in attempt is not waiting for a code"
            );
            audit(
                tx,
                &me.id.to_string(),
                "submit_application_authorization",
                &op.id.to_string(),
            )?;
            Ok((app, vm, op))
        })
    }
    pub fn resume_application_setup(&self, id: Uuid) -> Result<()> {
        self.transaction(|tx| {
            let mut op: ApplicationOperation = parse(tx.query_row(
                "SELECT record FROM application_operations WHERE id=?",
                [id.to_string()],
                |r| r.get(0),
            )?)?;
            let app = application(tx, op.application_id)?;
            if op.phase != ApplicationPhase::Interrupted
                || op.kind != ApplicationOperationKind::Setup
                || op.setup_action != ApplicationSetupAction::ConnectT3
                || app.observation.setup != "finishing"
                || !get_user(tx, op.actor)?.active
            {
                return Ok(());
            }
            op.phase = ApplicationPhase::Pending;
            tx.execute(
                "UPDATE application_operations SET phase=?,record=? WHERE id=?",
                params![op.phase.as_str(), json(&op)?, id.to_string()],
            )?;
            audit(
                tx,
                "controller",
                "resume_authorized_application_setup",
                &id.to_string(),
            )?;
            Ok(())
        })
    }
    pub fn record_application_report(&self, id: Uuid, mut report: ApplicationReport) -> Result<()> {
        // Browser URL/code are transient guest state, not host backup/audit data.
        report.observation.action = None;
        self.transaction(|tx| {
            let mut app = application(tx, id)?;
            app.observation = report.observation;
            tx.execute(
                "UPDATE applications SET record=? WHERE id=?",
                params![json(&app)?, id.to_string()],
            )?;
            if let Some(job) = report.job {
                let record: Option<String> = tx
                    .query_row(
                        "SELECT record FROM application_operations WHERE id=? AND application_id=?",
                        params![job.id.to_string(), id.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(record) = record {
                    let mut op: ApplicationOperation = parse(record)?;
                    // A process can finish after we observed its unit disappear.
                    // Accept confirmed setup success for that same job without
                    // reviving work or changing any VM resource commitments.
                    let recovered_success = op.phase == ApplicationPhase::Interrupted
                        && op.kind == ApplicationOperationKind::Setup
                        && job.phase == ApplicationPhase::Succeeded
                        && match op.setup_action {
                            ApplicationSetupAction::ConnectT3 => app.observation.setup == "ready",
                            ApplicationSetupAction::SignInCodex
                            | ApplicationSetupAction::SignInClaude => {
                                app.observation.components.iter().any(|c| {
                                    Some(c.name.as_str()) == op.setup_action.provider_name()
                                        && c.account == "signed_in"
                                })
                            }
                        }
                        && {
                            let vm = get_vm(tx, app.vm_id)?;
                            vm.desired != Lifecycle::Deleted && vm.observed != "deleted"
                        };
                    if op.phase.active() || recovered_success {
                        op.phase = job.phase;
                        tx.execute(
                            "UPDATE application_operations SET phase=?,record=? WHERE id=?",
                            params![op.phase.as_str(), json(&op)?, op.id.to_string()],
                        )?;
                    }
                }
            }
            Ok(())
        })
    }
}
