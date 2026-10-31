use super::*;
use crate::ApplicationRequest;
use iced::widget::{column, row};
use uuid::Uuid;

fn loading<'a>(label: impl Into<Cow<'a, str>>) -> Element<'a, Msg> {
    let indicator: Element<'a, Msg> = iced_m3::loading_indicator().size(28.0).into();
    row![indicator, copy(label)]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
}

fn title(target: ApplicationSetupAction) -> &'static str {
    match target {
        ApplicationSetupAction::ConnectT3 => "T3",
        ApplicationSetupAction::SignInCodex => "Codex",
        ApplicationSetupAction::SignInClaude => "Claude",
    }
}

fn active_for(
    operation: Option<&ApplicationOperation>,
    target: ApplicationSetupAction,
) -> Option<&ApplicationOperation> {
    operation.filter(|o| {
        o.kind == ApplicationOperationKind::Setup && o.setup_action == target && o.phase.active()
    })
}

fn request_for(
    request: Option<&ApplicationRequest>,
    id: Uuid,
    target: ApplicationSetupAction,
) -> bool {
    request.is_some_and(|r| r.id == id && r.target == Some(target))
}

fn flow<'a>(
    app: &'a App,
    application: &'a ManagedApplication,
    target: ApplicationSetupAction,
    operation: Option<&'a ApplicationOperation>,
) -> Option<Element<'a, Msg>> {
    let observation = &application.observation;
    let requesting = request_for(app.app_request.as_ref(), application.id, target);
    if requesting {
        let label = if app.app_request.as_ref().is_some_and(|r| r.submitting_code) {
            "Sending authorization code…".to_owned()
        } else {
            format!("Preparing {} sign-in…", title(target))
        };
        return Some(loading(label));
    }
    let op = active_for(operation, target)?;
    if app.submitted_code == Some(op.id)
        || (observation.operation_id == Some(op.id) && observation.operation_stage == "verifying")
    {
        return Some(loading(format!("Checking {} sign-in…", title(target))));
    }
    let action = observation
        .action
        .as_ref()
        .filter(|a| a.setup_action == target && observation.operation_id == Some(op.id));
    if let Some(action) = action {
        if action.expires_at <= unix_now() {
            return Some(copy(
                "This sign-in link expired. Waiting for the attempt to finish before retrying.",
            ));
        }
        let opened = app.browser_opened == Some(application.id);
        let mut content = column![
            typography(if opened { "Waiting for your browser approval" } else { "Your sign-in link is ready" }, Type::TitleMedium),
            copy(match target {
                ApplicationSetupAction::ConnectT3 => "Open T3’s page and confirm that its code matches the one below.",
                ApplicationSetupAction::SignInCodex => "Open OpenAI’s page, sign into ChatGPT, then enter this code.",
                ApplicationSetupAction::SignInClaude => "Open Claude’s page and sign in. Then paste the authorization code it gives you below.",
            }),
        ].spacing(12);
        if !action.code.is_empty() {
            content = content.push(
                row![
                    heading(&action.code),
                    quiet("Copy code", Msg::CopyAuthorization(application.id))
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            );
        }
        content = content.push(
            button(if opened {
                "Open sign-in page again"
            } else {
                "Continue in browser"
            })
            .on_press(Msg::OpenAuthorization(application.id))
            .disabled(app.busy || app.shutdown_blocks_work()),
        );
        if action.input_required {
            content = content
                .push(
                    iced_m3::text_field("Authorization code from Claude", &app.authorization_input)
                        .secure(true)
                        .on_input(Msg::AuthorizationInput),
                )
                .push(
                    button("Complete Claude sign-in")
                        .on_press(Msg::SubmitAuthorization(application.id, op.id))
                        .disabled(app.busy || app.authorization_input.trim().is_empty()),
                );
        }
        content = content.push(copy("This attempt is time-limited. You can leave this screen and return while it’s waiting."));
        return Some(content.into());
    }
    Some(loading(format!("Preparing {} sign-in…", title(target))))
}

fn status_dot<'a>(label: &'static str, good: bool, waiting: bool) -> Element<'a, Msg> {
    let state = if good {
        lifecycle::State::Running
    } else if waiting {
        lifecycle::State::Updating
    } else {
        lifecycle::State::Stopped
    };
    iced_m3::tooltip(lifecycle::marker(state), label).into()
}

fn account_caption(account: Option<&ApplicationAccount>, fallback: &str) -> String {
    let Some(account) = account else {
        return fallback.into();
    };
    let identity = account.email.as_deref().unwrap_or({
        if matches!(
            account.authentication.as_deref(),
            Some("apiKey" | "api_key" | "api_key_helper")
        ) {
            "API key authentication"
        } else {
            fallback
        }
    });
    match account.plan.as_deref() {
        Some(plan) => format!(
            "{identity} · {}",
            match plan {
                "pro" => "Pro",
                "max" => "Max",
                "plus" => "Plus",
                "free" => "Free",
                "team" => "Team",
                "business" => "Business",
                "enterprise" => "Enterprise",
                other => other,
            }
        ),
        None => identity.into(),
    }
}

pub(super) fn view<'a>(app: &'a App, snapshot: &'a Snapshot, vm_id: Uuid) -> Element<'a, Msg> {
    if let Some(vm) = snapshot
        .vms
        .iter()
        .find(|v| v.id == vm_id && v.spec.managed_host())
    {
        let application = snapshot
            .applications
            .iter()
            .find(|a| a.vm_id == vm_id && a.kind == ApplicationKind::HomecloudHost);
        let mut body = column![heading("Homecloud host"), copy("A separate controller with Incus and a ZFS test pool. Its accounts and state are independent of this host.")].spacing(16);
        if let Some(application) = application {
            let o = &application.observation;
            body = body.push(copy(o.message.clone()));
            if !o.operation_message.is_empty() {
                body = body.push(copy(o.operation_message.clone()));
            }
            for c in &o.components {
                body = body.push(
                    row![
                        status_dot(
                            if c.ready { "Ready" } else { "Unavailable" },
                            c.ready,
                            false
                        ),
                        copy(format!(
                            "{}{}",
                            c.name,
                            c.version
                                .as_ref()
                                .map(|v| format!(" · {v}"))
                                .unwrap_or_default()
                        ))
                    ]
                    .spacing(12),
                );
            }
            let active = snapshot
                .application_operations
                .iter()
                .any(|op| op.application_id == application.id && op.phase.active());
            if active {
                body = body.push(loading("Preparing host…"));
            } else if (o.installed_version.is_none() || o.components.iter().any(|c| !c.ready))
                && snapshot.me.role == Role::Admin
            {
                body = body.push(
                    button("Retry host installation")
                        .on_press(Msg::InstallManagedHost(vm.id))
                        .disabled(app.shutdown_blocks_work()),
                );
            }
        } else {
            body = body.push(copy("Host installation is queued."));
        }
        return surface(body, false).into();
    }

    let Some(vm) = snapshot.vms.iter().find(|v| v.id == vm_id) else {
        return copy("This VM is no longer available.");
    };
    let running = lifecycle::state(app, snapshot, vm) == lifecycle::State::Running;
    let owner = vm.owner == snapshot.me.id;
    let unavailable = if app.snapshot_unavailable || vm.observed == "unavailable" {
        "Status unavailable"
    } else if snapshot
        .memory_health
        .as_ref()
        .is_some_and(|h| h.waiting(vm))
    {
        "VM startup blocked by host policy"
    } else if vm.observed == "running" {
        "VM updating"
    } else if vm.desired == Lifecycle::Running {
        "VM starting"
    } else {
        "VM stopped"
    };
    let Some(application) = snapshot
        .applications
        .iter()
        .find(|a| a.vm_id == vm_id && a.kind == ApplicationKind::T3)
    else {
        let installing = app
            .app_request
            .as_ref()
            .is_some_and(|r| r.id == vm_id && r.target.is_none());
        let mut card = column![heading("Apps & accounts"), copy("Run T3 and its provider tools in this VM. Link a T3 account, then sign into a provider.")].spacing(16);
        card = if installing {
            card.push(loading("Submitting installation…"))
        } else {
            card.push(
                button("Install T3 Code")
                    .on_press(Msg::InstallApplication(vm_id))
                    .disabled(!running || app.busy || app.shutdown_blocks_work()),
            )
        };
        if !running {
            card = card.push(copy(unavailable));
        }
        return surface(card, false).into();
    };
    let observation = &application.observation;
    let operation = snapshot
        .application_operations
        .iter()
        .rev()
        .find(|op| op.application_id == application.id);
    let active = operation.is_some_and(|op| op.phase.active());
    let installing = app
        .app_request
        .as_ref()
        .is_some_and(|r| r.id == vm_id && r.target.is_none())
        || operation
            .is_some_and(|op| op.kind == ApplicationOperationKind::Install && op.phase.active());
    let installed = observation.installed_version.as_deref() == Some(&application.desired_version);
    let missing = !installed || observation.components.iter().any(|c| !c.ready);
    let connected = observation.setup == "ready";
    let mut content = column![heading("Apps & accounts")].spacing(16);
    if !running {
        content = content.push(copy(format!(
            "{unavailable}. Account information is from the last check."
        )));
    }
    if installing || missing {
        let mut tools = column![typography("Application tools", Type::TitleMedium)].spacing(12);
        if installing {
            tools = tools.push(loading("Installing T3 and provider tools…"));
        } else {
            tools = tools
                .push(copy("Install the tools before connecting accounts."))
                .push(
                    button(if installed {
                        "Install provider tools"
                    } else {
                        "Install T3 Code"
                    })
                    .on_press(Msg::InstallApplication(vm_id))
                    .disabled(!running || active || app.busy || app.shutdown_blocks_work()),
                );
        }
        content = content.push(tools);
    }
    let target = ApplicationSetupAction::ConnectT3;
    let progress = running
        .then(|| flow(app, application, target, operation))
        .flatten();
    let connection_status = if !running {
        unavailable
    } else if connected {
        "T3 account linked"
    } else if progress.is_some() {
        "Linking T3 account"
    } else {
        "T3 account not linked"
    };
    let mut header = row![
        status_dot(connection_status, running && connected, progress.is_some()),
        typography("T3 Code", Type::TitleMedium),
        horizontal_space()
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    if progress.is_none() && !connected && running && installed {
        header = header.push(
            button(
                if matches!(observation.setup.as_str(), "failed" | "interrupted") {
                    "Get a new authorization link"
                } else {
                    "Connect T3 account"
                },
            )
            .on_press(Msg::SetupApplication(application.id, target))
            .disabled(
                active || app.busy || app.shutdown_blocks_work() || app.app_request.is_some(),
            ),
        );
    }
    let mut connection = column![header].spacing(8);
    if let Some(progress) = progress {
        connection = connection.push(progress);
    } else if connected {
        if observation.message.contains("not currently confirmed") {
            connection = connection.push(copy(
                "Link saved; remote connectivity is not currently confirmed.",
            ));
        }
    } else if matches!(observation.setup.as_str(), "failed" | "interrupted") {
        connection = connection.push(copy(&observation.message));
    } else {
        connection = connection.push(copy("Link this VM to your T3 account for remote access."));
    }
    content = content.push(connection);
    let mut providers = column![].spacing(16);
    if !observation
        .components
        .iter()
        .any(|c| c.account == "signed_in")
    {
        providers = providers.push(copy(
            "Sign into at least one provider. Credentials stay inside this VM.",
        ));
    }
    for (label, name, target) in [
        ("Codex", "Codex CLI", ApplicationSetupAction::SignInCodex),
        (
            "Claude",
            "Claude Code",
            ApplicationSetupAction::SignInClaude,
        ),
    ] {
        let component = observation.components.iter().find(|c| c.name == name);
        let signed_in = component.is_some_and(|c| c.account == "signed_in");
        let progress = running
            .then(|| flow(app, application, target, operation))
            .flatten();
        let failed = operation.is_some_and(|op| {
            op.kind == ApplicationOperationKind::Setup
                && op.setup_action == target
                && matches!(
                    op.phase,
                    ApplicationPhase::Failed | ApplicationPhase::Interrupted
                )
        });
        let status = if !running {
            unavailable
        } else if progress.is_some() {
            "Signing in"
        } else if signed_in {
            "Signed in"
        } else if !component.is_some_and(|c| c.ready) {
            "Not installed"
        } else if component.is_some_and(|c| c.account == "signed_out") {
            "Not signed in"
        } else {
            "Checking account"
        };
        let account = owner
            .then(|| observation.accounts.iter().find(|a| a.name == name))
            .flatten();
        let identity = row![
            status_dot(
                status,
                running && signed_in,
                progress.is_some() || status == "Checking account"
            ),
            typography(label, Type::TitleMedium)
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let mut header = row![identity].spacing(12).align_y(Alignment::Center);
        if signed_in && owner {
            header = header.push(copy(account_caption(
                account,
                "Signed in · account email unavailable",
            )));
        } else {
            header = header.push(horizontal_space());
        }
        if owner
            && running
            && component.is_some_and(|c| c.ready && c.account == "signed_out")
            && progress.is_none()
        {
            header = header.push(
                button(match (target, failed) {
                    (ApplicationSetupAction::SignInCodex, false) => "Sign into Codex",
                    (ApplicationSetupAction::SignInCodex, true) => "Retry Codex sign-in",
                    (ApplicationSetupAction::SignInClaude, false) => "Sign into Claude",
                    _ => "Retry Claude sign-in",
                })
                .on_press(Msg::SetupApplication(application.id, target))
                .disabled(
                    active || app.busy || app.shutdown_blocks_work() || app.app_request.is_some(),
                ),
            );
        }
        let mut provider = column![header.wrap()].spacing(8);
        if signed_in && !owner {
            provider = provider.push(copy("The VM owner manages this provider account."));
        } else if !signed_in && progress.is_none() {
            provider = provider.push(copy(status));
        }
        if let Some(progress) = progress {
            provider = provider.push(progress);
        } else if failed && !signed_in && observation.operation_id == operation.map(|op| op.id) {
            provider = provider.push(copy(&observation.operation_message));
        }
        providers = providers.push(provider);
    }
    surface(content.push(providers), false).padding(20).into()
}

pub(super) fn details<'a>(app: &'a App, application: &'a ManagedApplication) -> Element<'a, Msg> {
    let observation = &application.observation;
    let mut content = column![quiet(
        if app.app_details {
            "Hide application details"
        } else {
            "Show application details"
        },
        Msg::ToggleAppDetails
    )]
    .spacing(8);
    if app.app_details {
        content = content
            .push(copy(format!(
                "T3 version · {}",
                observation
                    .installed_version
                    .as_deref()
                    .unwrap_or("Not installed")
            )))
            .push(copy(format!(
                "Service · {} · Starts at boot",
                observation.service
            )));
        for account in observation.accounts.iter().filter(|_| {
            app.snapshot.as_ref().is_some_and(|s| {
                s.vms
                    .iter()
                    .any(|v| v.id == application.vm_id && v.owner == s.me.id)
            })
        }) {
            if let Some(id) = &account.account_id {
                content = content.push(copy(format!("{} account ID · {id}", account.name)));
            }
        }
        for component in &observation.components {
            content = content.push(copy(format!(
                "{} · {}",
                component.name,
                component.version.as_deref().unwrap_or("Not installed")
            )));
        }
    }
    content.into()
}
