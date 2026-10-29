use super::*;

impl Service {
    /// Independent of the VM resource-operation lane. Guest jobs outlive this call.
    pub async fn reconcile_applications_once(&self) -> Result<()> {
        let _guard = self.application_reconciliation.lock().await;
        let vms = self.db(|s| s.vms()).await?;
        let vm_operations = self.db(|s| s.operations()).await?;
        let operations = self.db(|s| s.application_operations()).await?;
        for app in self.db(|s| s.applications()).await? {
            let Some(vm) = vms.iter().find(|v| v.id == app.vm_id) else {
                continue;
            };
            let active = operations
                .iter()
                .find(|op| op.application_id == app.id && op.phase.active());
            if vm.desired != Lifecycle::Running || vm.observed != "running" {
                self.application_actions.lock().await.remove(&app.id);
                let mut observation = app.observation.clone();
                observation.action = None;
                observation.service = "unavailable".into();
                observation.message = "Start the VM to inspect its applications".into();
                let job = active
                    .filter(|op| {
                        op.kind == ApplicationOperationKind::Setup
                            || vm.desired == Lifecycle::Deleted
                    })
                    .map(|op| {
                        if op.setup_action == ApplicationSetupAction::ConnectT3 {
                            observation.setup = "interrupted".into();
                        }
                        observation.operation_message =
                            "Setup interrupted because the VM stopped. Request a new sign-in link."
                                .into();
                        ApplicationJob {
                            id: op.id,
                            phase: ApplicationPhase::Interrupted,
                        }
                    });
                let id = app.id;
                self.db(move |s| {
                    s.record_application_report(id, ApplicationReport { observation, job })
                })
                .await?;
                continue;
            }
            // Do not compete with an in-flight VM restart, resize, or deletion.
            if vm_operations
                .iter()
                .any(|o| o.vm_id == vm.id && !matches!(o.phase.as_str(), "succeeded" | "failed"))
            {
                continue;
            }
            let command = match active {
                Some(op) if op.kind == ApplicationOperationKind::Install => {
                    ApplicationCommand::EnsureInstalled {
                        operation: op.clone(),
                    }
                }
                Some(op) if op.phase == ApplicationPhase::Pending => {
                    ApplicationCommand::StartSetup {
                        operation: op.clone(),
                    }
                }
                _ => ApplicationCommand::Inspect {
                    operation: active.cloned(),
                },
            };
            let result = self
                .backend
                .request(HostRequest::Application {
                    vm: vm.clone(),
                    application: Box::new(app.clone()),
                    command,
                })
                .await;
            match result {
                Ok(HostResponse::Application(report)) => {
                    let mut actions = self.application_actions.lock().await;
                    actions.remove(&app.id);
                    if let Some(action) = &report.observation.action
                        && action.expires_at > homecloud_store::now()
                    {
                        actions.insert(app.id, action.clone());
                    }
                    drop(actions);
                    let id = app.id;
                    self.db(move |s| {
                        let resume = report
                            .job
                            .as_ref()
                            .filter(|j| {
                                j.phase == ApplicationPhase::Interrupted
                                    && report.observation.setup == "finishing"
                            })
                            .map(|j| j.id);
                        s.record_application_report(id, report)?;
                        if let Some(operation) = resume {
                            s.resume_application_setup(operation)?;
                        }
                        Ok(())
                    })
                    .await?;
                }
                _ => {
                    // Never persist raw guest CLI output: it may contain an auth challenge.
                    self.application_actions.lock().await.remove(&app.id);
                    let mut observation = app.observation.clone();
                    observation.action = None;
                    observation.service = "unknown".into();
                    observation.message =
                        "Guest application inspection unavailable; retrying".into();
                    let id = app.id;
                    self.db(move |s| {
                        s.record_application_report(
                            id,
                            ApplicationReport {
                                observation,
                                job: None,
                            },
                        )
                    })
                    .await?;
                }
            }
        }
        Ok(())
    }
}
