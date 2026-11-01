//! Inspection and supervised jobs for the T3 managed application. Runs as
//! the guest owner and never listens on a network socket.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use homecloud_core::{
    ApplicationAccount, ApplicationComponent, ApplicationJob, ApplicationObservation,
    ApplicationOperationKind, ApplicationPhase, ApplicationReport, ApplicationSetupAction,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ctx::Ctx;
use crate::error::{Error, Result};
use crate::fsutil::{private_dir, read_json, save_json, stamp_of, try_lock};
use crate::metadata::{AccountInfo, AccountState};
use crate::process;
use crate::providers::{PROVIDERS, Provider};
use crate::state::{Job, JobState};
use crate::text::{display_text, find_version, parse_version};
use crate::{claude, codex, github, install, t3};

/// Account caches are reused this long, so the inspection loop doesn't start
/// a provider CLI every few seconds.
const ACCOUNT_CACHE_SECONDS: f64 = 10.0;

pub fn interrupted_message(action: ApplicationSetupAction) -> &'static str {
    match action {
        ApplicationSetupAction::SignInGitHub => {
            "GitHub sign-in was interrupted. Request a new sign-in link."
        }
        ApplicationSetupAction::SignOutGitHub => {
            "Removing GitHub access was interrupted. Try again."
        }
        ApplicationSetupAction::ConnectT3 => "Setup interrupted. Request a new authorization link.",
        _ => "Provider sign-in was interrupted. Request a new sign-in link.",
    }
}

pub fn success_message(action: ApplicationSetupAction, component: &str) -> String {
    match action {
        ApplicationSetupAction::SignInGitHub => {
            "GitHub sign-in saved in this VM. Git and gh now use this account.".into()
        }
        ApplicationSetupAction::SignOutGitHub => {
            "GitHub access removed from this VM. Its token stays valid until revoked in GitHub settings.".into()
        }
        _ => format!("{component} sign-in saved in this VM. Refresh provider checks in T3 Code."),
    }
}

/// The provider whose account a setup action changes.
fn provider_for(action: ApplicationSetupAction) -> Option<&'static Provider> {
    Provider::by_name(action.provider_name()?)
}

fn job_path(ctx: &Ctx, app: Uuid) -> PathBuf {
    ctx.base().join(app.to_string()).join("job.json")
}

#[derive(Serialize, Deserialize)]
struct VersionCache {
    stamp: (String, i64, u64),
    version: String,
}

#[derive(Serialize, Deserialize)]
struct AccountCache {
    account: AccountState,
    metadata: AccountInfo,
    observed_at: f64,
}

fn provider_version(
    ctx: &Ctx,
    directory: &Path,
    provider: &Provider,
    path: &Path,
) -> Option<String> {
    let stamp = stamp_of(path)?;
    let cache_path = directory.join(format!("{}-version.json", provider.binary));
    if let Some(cache) = read_json::<VersionCache>(&cache_path).filter(|cache| cache.stamp == stamp)
    {
        return Some(cache.version);
    }
    let mut command = ctx.command(path);
    command.arg("--version");
    let output = process::capture(command, Duration::from_secs(30), 65536).ok()??;
    if !output.success() {
        return None;
    }
    let version = find_version(&output.text(), false)?;
    let _ = save_json(
        &cache_path,
        &VersionCache {
            stamp,
            version: version.clone(),
        },
    );
    Some(version)
}

/// Provider tools with their versions and account state. `completed` holds,
/// per binary, when a sign-in last finished; older account caches are stale.
pub fn provider_components(
    ctx: &Ctx,
    directory: &Path,
    completed: &BTreeMap<&str, f64>,
) -> Vec<(ApplicationComponent, Option<ApplicationAccount>)> {
    PROVIDERS
        .iter()
        .map(|provider| {
            let found = ctx
                .which(provider.binary)
                .and_then(|path| provider_version(ctx, directory, provider, &path));
            let ready = found
                .as_deref()
                .and_then(parse_version)
                .is_some_and(|version| version >= provider.minimum);
            let mut checked_at = crate::now_f64();
            let mut info = AccountInfo::from(AccountState::Unavailable);
            if ready {
                let cache_path = directory.join(format!("{}-account.json", provider.binary));
                let since = completed.get(provider.binary).copied().unwrap_or(0.0);
                let cached = read_json::<AccountCache>(&cache_path).filter(|cache| {
                    let age = checked_at - cache.observed_at;
                    cache.observed_at >= since && (0.0..ACCOUNT_CACHE_SECONDS).contains(&age)
                });
                if let Some(cache) = cached {
                    info = cache.metadata;
                    checked_at = cache.observed_at;
                } else {
                    info = provider.account_info(ctx);
                    let cache = AccountCache {
                        account: info.state,
                        metadata: info.clone(),
                        observed_at: checked_at,
                    };
                    let _ = save_json(&cache_path, &cache);
                }
            }
            let account = (info.state == AccountState::SignedIn).then(|| ApplicationAccount {
                name: provider.name.into(),
                email: info.email.clone(),
                account_id: info.account_id.clone(),
                username: info.username.clone(),
                plan: info.plan.clone(),
                authentication: info.authentication.clone(),
                observed_at: checked_at as u64,
            });
            let component = ApplicationComponent {
                name: provider.name.into(),
                version: found,
                ready,
                account: info.state.as_str().into(),
            };
            (component, account)
        })
        .collect()
}

fn unit_active(ctx: &Ctx, unit: &str) -> Option<bool> {
    let mut command = ctx.tool("systemctl");
    command.args(["is-active", "--quiet", unit]);
    let output = process::capture(command, Duration::from_secs(15), 4096).ok()??;
    Some(output.success())
}

fn service_running(ctx: &Ctx) -> bool {
    let mut command = ctx.tool("systemctl");
    command.args(["--user", "is-active", "t3code.service"]);
    process::capture(command, Duration::from_secs(15), 4096)
        .ok()
        .flatten()
        .is_some_and(|output| output.success())
}

pub fn inspect(ctx: &Ctx, app: Uuid) -> Result<ApplicationReport> {
    let path = job_path(ctx, app);
    let directory = path.parent().expect("job directory").to_path_buf();
    let mut state: Option<JobState> = read_json(&path);
    if let Some(job) = state.as_mut().filter(|job| job.active()) {
        // A reboot, or a stopped or failed unit, can't leave an immortal operation.
        let rebooted = ctx.boot_id().ok().is_some_and(|boot| boot != job.boot_id);
        if rebooted || unit_active(ctx, &job.unit) == Some(false) {
            job.phase = ApplicationPhase::Interrupted;
            job.action = None;
            job.message = Some(interrupted_message(job.setup_action).into());
            save_json(&path, job)?;
        }
    }
    let version = t3::version(ctx, &directory);
    let service = if service_running(ctx) {
        "running"
    } else {
        "stopped"
    };
    let status = t3::status(ctx);
    let mut setup = if version.is_some() && status.is_none() {
        "unknown"
    } else {
        "required"
    };
    let mut message = if version.is_none() {
        "Install T3 Code to get started"
    } else {
        "Connect your T3 account"
    };
    if let Some(status) = &status {
        if status.revoked() {
            message = "T3 account connection was revoked. Connect again to authorize this VM.";
        } else if status.linked() {
            setup = "ready";
            message = if status.reachable() {
                "Linked to T3. Sign into the same T3 account on another device."
            } else {
                "Account connected; remote connectivity is not currently confirmed."
            };
        } else if status.authenticated() {
            setup = "finishing";
            message = "Account authorization saved; finishing connection";
        }
    }
    if version.is_some() && status.is_none() {
        message = "Could not inspect the T3 connection; retrying";
    }
    let mut message = message.to_owned();

    let mut completed = BTreeMap::new();
    if let Some(job) = state
        .as_ref()
        .filter(|job| job.phase == ApplicationPhase::Succeeded)
        && let Some(provider) = provider_for(job.setup_action)
    {
        completed.insert(provider.binary, job.completed_at.unwrap_or(0.0));
    }
    let (components, accounts): (Vec<_>, Vec<_>) = provider_components(ctx, &directory, &completed)
        .into_iter()
        .unzip();
    let mut accounts: Vec<ApplicationAccount> = accounts.into_iter().flatten().collect();
    if let Some(status) = status.as_ref().filter(|status| status.authenticated())
        && let Some(id) = display_text(status.cloud_user_id(), 128)
    {
        accounts.insert(
            0,
            ApplicationAccount {
                name: "T3".into(),
                account_id: Some(id),
                observed_at: crate::now(),
                ..ApplicationAccount::default()
            },
        );
    }

    let mut action = None;
    let mut operation_message = String::new();
    if let Some(job) = state.as_mut() {
        let target = job.setup_action;
        let provider = target
            .provider_name()
            .filter(|_| job.kind == ApplicationOperationKind::Setup);
        let confirmed = match provider {
            Some(name) => components
                .iter()
                .any(|c| c.name == name && Some(c.account.as_str()) == target.expected_account()),
            None => setup == "ready",
        };
        // Recovery only repairs the matching target: T3 being linked says
        // nothing about whether Codex credentials were ever saved.
        if confirmed
            && (target != ApplicationSetupAction::SignInGitHub || job.stage != "preparing")
            && job.kind == ApplicationOperationKind::Setup
            && matches!(
                job.phase,
                ApplicationPhase::Interrupted | ApplicationPhase::Failed
            )
        {
            job.phase = ApplicationPhase::Succeeded;
            job.action = None;
            save_json(&path, job)?;
        }
        match job.phase {
            ApplicationPhase::WaitingForUser => {
                if let Some(candidate) = job.action.clone().filter(|a| a.expires_at > crate::now())
                {
                    action = Some(homecloud_core::BrowserAction {
                        setup_action: target,
                        ..candidate
                    });
                    if provider.is_none() {
                        setup = "waiting_for_user";
                        message = "Approve this VM in your browser".into();
                    }
                }
            }
            ApplicationPhase::Running if provider.is_none() => {
                setup = "in_progress";
                message = if job.kind == ApplicationOperationKind::Install {
                    "Installing T3 Code"
                } else {
                    "Finishing T3 connection"
                }
                .into();
            }
            ApplicationPhase::Failed | ApplicationPhase::Interrupted => {
                operation_message = job
                    .message
                    .clone()
                    .unwrap_or_else(|| "Setup interrupted. Request a new sign-in link.".into());
                if provider.is_none() && !matches!(setup, "ready" | "finishing") {
                    setup = job.phase.as_str();
                    message = operation_message.clone();
                }
            }
            _ => {}
        }
        if let Some(name) = provider
            && job.phase == ApplicationPhase::Succeeded
        {
            operation_message = success_message(target, name);
        }
    }
    Ok(ApplicationReport {
        observation: ApplicationObservation {
            accounts,
            installed_version: version,
            components,
            service: service.into(),
            setup: setup.into(),
            message,
            observed_at: crate::now(),
            action,
            operation_message,
            operation_id: state.as_ref().map(|job| job.id),
            operation_stage: state
                .as_ref()
                .map(|job| job.stage.clone())
                .unwrap_or_default(),
        },
        job: state.map(|job| ApplicationJob {
            id: job.id,
            phase: job.phase,
        }),
    })
}

/// One supervised operation, run in its own transient systemd unit.
pub fn job(
    ctx: &Ctx,
    app: Uuid,
    id: Uuid,
    kind: ApplicationOperationKind,
    version: &str,
    unit: &str,
    action: ApplicationSetupAction,
) -> Result<()> {
    let path = job_path(ctx, app);
    let directory = path.parent().expect("job directory").to_path_buf();
    private_dir(&directory)?;
    let Some(_lock) = try_lock(&directory.join("worker.lock"))? else {
        return Err(Error::Internal(
            "another job is running for this application".into(),
        ));
    };
    let mut job = Job::new(
        path,
        JobState {
            id,
            kind,
            phase: ApplicationPhase::Running,
            unit: unit.into(),
            boot_id: ctx.boot_id()?,
            setup_action: action,
            stage: "preparing".into(),
            action: None,
            message: None,
            completed_at: None,
        },
    );
    job.save()?;
    let outcome = run(ctx, &mut job, kind, version, action);
    if let Some(provider) = provider_for(action)
        && (outcome.is_ok() || provider.binary == "gh")
    {
        // The next inspection reads the account afresh.
        let _ = fs::remove_file(directory.join(format!("{}-account.json", provider.binary)));
    }
    job.state.action = None;
    match outcome {
        Ok(()) => {
            job.state.phase = ApplicationPhase::Succeeded;
            job.state.stage = "complete".into();
            job.state.completed_at = Some(crate::now_f64());
        }
        Err(error) => {
            eprintln!("job failed: {error}");
            job.state.phase = ApplicationPhase::Failed;
            job.state.message = Some(error.user_message().into());
        }
    }
    job.save()
}

fn run(
    ctx: &Ctx,
    job: &mut Job,
    kind: ApplicationOperationKind,
    version: &str,
    action: ApplicationSetupAction,
) -> Result<()> {
    if kind == ApplicationOperationKind::Install {
        return install::install(ctx, version);
    }
    match action {
        ApplicationSetupAction::ConnectT3 => t3::connect(ctx, job),
        ApplicationSetupAction::SignInCodex => codex::authorize(ctx, job),
        ApplicationSetupAction::SignInClaude => claude::authorize(ctx, job),
        ApplicationSetupAction::SignInGitHub => github::authorize(ctx, job),
        ApplicationSetupAction::SignOutGitHub => github::sign_out(ctx),
    }
}

/// Hands a browser-returned authorization code to the waiting sign-in.
pub fn submit(ctx: &Ctx, app: Uuid, id: Uuid, code: Option<&str>) -> Result<()> {
    let path = job_path(ctx, app);
    claude::submit(ctx, path.parent().expect("job directory"), id, code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;
    use homecloud_core::BrowserAction;

    const APP: Uuid = Uuid::from_u128(1);

    struct Scenario {
        fx: Fixture,
    }

    /// A guest with T3 installed, a T3 status, and a unit that is (not) running.
    fn scenario(status: &str, unit_active: bool) -> Scenario {
        let fx = Fixture::new();
        fx.write("boot", "new");
        let bin = fx.ctx.bin_dir();
        let t3 = format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 't3 v0.1.2'; exit 0; fi\nif [ \"$1\" = connect ]; then echo '{status}'; exit 0; fi\nexit 1\n"
        );
        fx.script(&bin.join("t3"), &t3);
        let systemctl = format!(
            "#!/bin/sh\nif [ \"$1\" = --user ]; then exit 0; fi\nexit {}\n",
            if unit_active { 0 } else { 3 }
        );
        fx.tool("systemctl", &systemctl);
        Scenario { fx }
    }

    impl Scenario {
        fn provider(&self, name: &str, version: &str, account: &str) {
            let bin = self.fx.ctx.bin_dir();
            let body = match name {
                "gh" => format!(
                    "if [ \"$1\" = config ]; then {}; fi",
                    if account == "signed_in" {
                        "echo octo-fixture; exit 0"
                    } else {
                        "printf 'could not find key \"user\"\\n' >&2; exit 1"
                    }
                ),
                "claude" => format!(
                    "if [ \"$1\" = auth ]; then echo '{{\"loggedIn\": {}}}'; exit {}; fi",
                    account == "signed_in",
                    if account == "signed_in" { 0 } else { 1 }
                ),
                _ => "exit 1".into(),
            };
            let script = format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '{name} {version}'; exit 0; fi\n{body}\n"
            );
            self.fx.script(&bin.join(name), &script);
        }

        fn save_job(&self, phase: ApplicationPhase, action: ApplicationSetupAction, boot: &str) {
            let mut job = crate::state::test_job(&self.fx.ctx.base().join(APP.to_string()), action);
            job.state.phase = phase;
            job.state.boot_id = boot.into();
            if action == ApplicationSetupAction::SignInGitHub {
                job.state.stage = "awaiting_approval".into();
            }
            job.state.action = Some(BrowserAction {
                url: "https://example.com".into(),
                code: "OLD-CODE".into(),
                expires_at: 99_999_999_999,
                setup_action: ApplicationSetupAction::ConnectT3,
                input_required: false,
            });
            job.save().unwrap();
        }

        fn inspect(&self) -> ApplicationReport {
            inspect(&self.fx.ctx, APP).unwrap()
        }
    }

    const UNLINKED: &str = r#"{"authenticated": false, "desired": false, "linked": false}"#;
    const AUTHENTICATED: &str = r#"{"authenticated": true, "desired": false, "linked": false}"#;
    const LINKED: &str = r#"{"authenticated": true, "desired": true, "linked": true, "cloudUserId": "user-fixture"}"#;
    const REVOKED: &str =
        r#"{"authenticated": true, "desired": true, "linked": true, "revoked": true}"#;

    fn after_restart(status: &str, boot: &str, active: bool) -> ApplicationReport {
        let s = scenario(status, active);
        s.save_job(
            ApplicationPhase::WaitingForUser,
            ApplicationSetupAction::ConnectT3,
            boot,
        );
        s.inspect()
    }

    #[test]
    fn gui_or_controller_restart_preserves_live_challenge() {
        let report = after_restart(UNLINKED, "new", true);
        assert_eq!(
            report.observation.installed_version.as_deref(),
            Some("0.1.2")
        );
        assert_eq!(report.job.unwrap().phase, ApplicationPhase::WaitingForUser);
        assert_eq!(report.observation.action.unwrap().code, "OLD-CODE");
        assert_eq!(report.observation.setup, "waiting_for_user");
    }

    #[test]
    fn vm_reboot_or_dead_job_clears_incomplete_pairing() {
        for (boot, active) in [("old", true), ("new", false)] {
            let report = after_restart(UNLINKED, boot, active);
            assert_eq!(report.job.unwrap().phase, ApplicationPhase::Interrupted);
            assert!(report.observation.action.is_none());
            assert_eq!(report.observation.setup, "interrupted");
        }
    }

    #[test]
    fn saved_credentials_allow_recovery_without_a_new_challenge() {
        let report = after_restart(AUTHENTICATED, "old", true);
        assert_eq!(report.observation.setup, "finishing");
        assert!(report.observation.action.is_none());
    }

    #[test]
    fn success_before_crash_is_observed_without_repeating_pairing() {
        let report = after_restart(LINKED, "old", true);
        assert_eq!(report.job.unwrap().phase, ApplicationPhase::Succeeded);
        assert_eq!(report.observation.setup, "ready");
        assert_eq!(
            report.observation.accounts[0].account_id.as_deref(),
            Some("user-fixture")
        );
    }

    #[test]
    fn revoked_credential_is_not_automatically_relinked() {
        let report = after_restart(REVOKED, "old", true);
        assert!(!matches!(
            report.observation.setup.as_str(),
            "finishing" | "ready"
        ));
    }

    #[test]
    fn pairing_success_does_not_hide_missing_provider_binaries() {
        let report = after_restart(LINKED, "old", true);
        assert_eq!(report.observation.setup, "ready");
        let names: Vec<&str> = report
            .observation
            .components
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["Codex CLI", "Claude Code", "GitHub CLI"]);
        assert!(
            report
                .observation
                .components
                .iter()
                .all(|c| !c.ready && c.version.is_none())
        );
    }

    #[test]
    fn missing_t3_and_unreadable_status_have_distinct_states() {
        let s = scenario("not json", true);
        let report = s.inspect();
        assert_eq!(report.observation.setup, "unknown");
        assert_eq!(
            report.observation.message,
            "Could not inspect the T3 connection; retrying"
        );
        fs::remove_file(s.fx.ctx.t3()).unwrap();
        let report = s.inspect();
        assert_eq!(
            (
                report.observation.setup.as_str(),
                report.observation.installed_version
            ),
            ("required", None)
        );
        assert!(report.job.is_none());
    }

    fn provider_job(
        phase: ApplicationPhase,
        action: ApplicationSetupAction,
        account: &str,
        boot: &str,
        active: bool,
    ) -> ApplicationReport {
        let s = scenario(LINKED, active);
        s.provider("gh", "2.103.3", account);
        s.provider("claude", "2.1.308", account);
        s.save_job(phase, action, boot);
        s.inspect()
    }

    #[test]
    fn t3_connection_does_not_complete_a_pending_provider_login() {
        let report = provider_job(
            ApplicationPhase::WaitingForUser,
            ApplicationSetupAction::SignInClaude,
            "signed_out",
            "new",
            true,
        );
        assert_eq!(report.observation.setup, "ready");
        assert_eq!(report.job.unwrap().phase, ApplicationPhase::WaitingForUser);
        assert_eq!(
            report.observation.action.unwrap().setup_action,
            ApplicationSetupAction::SignInClaude
        );
    }

    #[test]
    fn reboot_clears_a_provider_challenge_without_unpairing_t3() {
        let report = provider_job(
            ApplicationPhase::WaitingForUser,
            ApplicationSetupAction::SignInClaude,
            "signed_out",
            "old",
            true,
        );
        assert_eq!(report.observation.setup, "ready");
        assert_eq!(report.job.unwrap().phase, ApplicationPhase::Interrupted);
        assert!(report.observation.action.is_none());
        assert!(report.observation.operation_message.contains("interrupted"));
    }

    #[test]
    fn committed_credentials_repair_an_interrupted_job() {
        let report = provider_job(
            ApplicationPhase::WaitingForUser,
            ApplicationSetupAction::SignInClaude,
            "signed_in",
            "new",
            false,
        );
        assert_eq!(report.job.unwrap().phase, ApplicationPhase::Succeeded);
        assert!(report.observation.action.is_none());
        assert!(
            report
                .observation
                .operation_message
                .contains("Claude Code sign-in saved")
        );
    }

    #[test]
    fn github_sign_in_and_removal_recover_only_from_their_own_outcome() {
        use ApplicationSetupAction::{SignInGitHub, SignOutGitHub};
        for (target, account, phase) in [
            (SignInGitHub, "signed_in", ApplicationPhase::Succeeded),
            (SignInGitHub, "signed_out", ApplicationPhase::Interrupted),
            (SignOutGitHub, "signed_out", ApplicationPhase::Succeeded),
            (SignOutGitHub, "signed_in", ApplicationPhase::Interrupted),
        ] {
            let report = provider_job(ApplicationPhase::Running, target, account, "new", false);
            assert_eq!(report.observation.setup, "ready");
            assert_eq!(report.job.unwrap().phase, phase, "{target:?} {account}");
            let message = report.observation.operation_message;
            let expected = match (phase, target) {
                (ApplicationPhase::Interrupted, SignOutGitHub) => "Removing GitHub access",
                (ApplicationPhase::Interrupted, _) => "sign-in was interrupted",
                (_, SignOutGitHub) => "revoked in GitHub settings",
                _ => "Git and gh now use this account",
            };
            assert!(message.contains(expected), "{message}");
        }
    }

    #[test]
    fn github_preparation_failure_is_not_recovered_from_an_existing_account() {
        let scenario = scenario(LINKED, false);
        scenario.provider("gh", "2.103.3", "signed_in");
        let mut job = crate::state::test_job(
            &scenario.fx.ctx.base().join(APP.to_string()),
            ApplicationSetupAction::SignInGitHub,
        );
        job.state.phase = ApplicationPhase::Failed;
        job.state.boot_id = "new".into();
        job.state.message = Some("Git configuration was refused.".into());
        job.save().unwrap();
        let report = scenario.inspect();
        assert_eq!(report.job.unwrap().phase, ApplicationPhase::Failed);
        assert_eq!(
            report.observation.operation_message,
            "Git configuration was refused."
        );
    }

    #[test]
    fn signed_in_account_metadata_is_published_with_its_component() {
        let s = scenario(LINKED, true);
        s.provider("gh", "2.103.3", "signed_in");
        let report = s.inspect();
        let gh = report
            .observation
            .components
            .iter()
            .find(|c| c.name == "GitHub CLI")
            .unwrap();
        assert_eq!(
            (gh.version.as_deref(), gh.ready, gh.account.as_str()),
            (Some("2.103.3"), true, "signed_in")
        );
        let account = report
            .observation
            .accounts
            .iter()
            .find(|a| a.name == "GitHub CLI")
            .unwrap();
        assert_eq!(account.username.as_deref(), Some("octo-fixture"));
        let codex = &report.observation.components[0];
        assert_eq!(
            (codex.ready, codex.account.as_str()),
            (false, "unavailable")
        );
    }

    #[test]
    fn completed_login_cannot_reuse_an_earlier_signed_out_observation() {
        let s = scenario(LINKED, true);
        s.provider("gh", "2.103.3", "signed_in");
        s.provider("claude", "2.1.308", "signed_in");
        let directory = s.fx.ctx.base().join(APP.to_string());
        for binary in ["gh", "claude"] {
            let provider = PROVIDERS
                .iter()
                .find(|provider| provider.binary == binary)
                .unwrap();
            provider_version(
                &s.fx.ctx,
                &directory,
                provider,
                &s.fx.ctx.bin_dir().join(binary),
            );
        }
        let now = crate::now_f64();
        for name in ["gh", "claude"] {
            let cache = AccountCache {
                account: AccountState::SignedOut,
                metadata: AccountState::SignedOut.into(),
                observed_at: if name == "claude" { now - 1.0 } else { now },
            };
            save_json(&directory.join(format!("{name}-account.json")), &cache).unwrap();
        }
        let completed = BTreeMap::from([("claude", now)]);
        let components = provider_components(&s.fx.ctx, &directory, &completed);
        assert_eq!(
            components[1].0.account, "signed_in",
            "the cache predates the completed sign-in"
        );
        assert_eq!(
            components[2].0.account, "signed_out",
            "an unrelated recent cache is reused"
        );
    }

    #[test]
    fn version_cache_follows_the_installed_binary() {
        let s = scenario(LINKED, true);
        s.provider("gh", "2.103.3", "signed_out");
        let directory = s.fx.ctx.base().join(APP.to_string());
        let gh = s.fx.ctx.bin_dir().join("gh");
        let first = provider_version(&s.fx.ctx, &directory, &PROVIDERS[2], &gh);
        assert_eq!(first.as_deref(), Some("2.103.3"));
        s.provider("gh", "2.110.10", "signed_out");
        assert_eq!(
            provider_version(&s.fx.ctx, &directory, &PROVIDERS[2], &gh).as_deref(),
            Some("2.110.10")
        );
    }
}
