//! GitHub CLI device-flow sign-in and sign-out.
//!
//! gh owns OAuth and its credential file. Homecloud reads only the active
//! login through gh's configuration interface (never the token) and the public
//! device challenge. Git is pointed at gh's credential helper before any token
//! exists.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use homecloud_core::{ApplicationSetupAction, BrowserAction};
use serde_json::Value;

use crate::ctx::Ctx;
use crate::error::{Result, fail};
use crate::fsutil::{read_json, save_json};
use crate::metadata::{AccountInfo, AccountState};
use crate::process::{self, Guard};
use crate::state::Job;
use crate::text::display_text;

const HOST: &str = "github.com";
const DEVICE_URL: &str = "https://github.com/login/device";
const MAX_OUTPUT: usize = 65536;
/// Tokens in the environment would override, and hide, the stored sign-in.
const IGNORED_ENV: [&str; 5] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
];
const FAILURE: &str = "GitHub sign-in did not finish. The code may have expired or been cancelled. Start again for a new code.";
const EXPIRED: &str = "GitHub sign-in attempt expired. Start again for a new code.";
const UNRESPONSIVE: &str = "The GitHub CLI did not respond. Try again.";

fn identity_path(ctx: &Ctx) -> PathBuf {
    ctx.home.join(".local/state/homecloud/github-identity.json")
}

fn command(ctx: &Ctx, program: &Path) -> Command {
    let mut command = ctx.command(program);
    for name in IGNORED_ENV {
        command.env_remove(name);
    }
    command.envs([
        ("GH_PROMPT_DISABLED", "1"),
        ("GH_NO_UPDATE_NOTIFIER", "1"),
        ("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1"),
        ("GH_SPINNER_DISABLED", "1"),
        ("NO_COLOR", "1"),
    ]);
    command
}

/// Runs a fixed command; output is bounded and never forwarded verbatim.
fn run(
    ctx: &Ctx,
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<(Option<i32>, String)> {
    let mut command = command(ctx, program);
    command.args(args);
    match process::capture(command, timeout, MAX_OUTPUT)? {
        Some(output) => Ok((output.code, output.text())),
        None => fail(UNRESPONSIVE),
    }
}

fn gh(ctx: &Ctx, args: &[&str], timeout: Duration) -> Result<(Option<i32>, String)> {
    let Some(binary) = ctx.which("gh") else {
        return fail("Install the GitHub CLI before signing in.");
    };
    run(ctx, &binary, args, timeout)
}

/// Includes Enterprise Managed User handles such as `octocat_acme`.
fn valid_login(login: &str) -> bool {
    let mut chars = login.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && login.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Local only: gh's saved active login, not a network check of its token.
pub fn account_info(ctx: &Ctx) -> AccountInfo {
    if ctx.which("gh").is_none() {
        return AccountState::Unavailable.into();
    }
    let Ok((code, out)) = gh(
        ctx,
        &["config", "get", "--host", HOST, "user"],
        Duration::from_secs(15),
    ) else {
        return AccountState::Unknown.into();
    };
    let login = out.trim();
    if code == Some(0) && valid_login(login) {
        return AccountInfo {
            username: Some(login.to_owned()),
            ..AccountState::SignedIn.into()
        };
    }
    if code == Some(1) && login.is_empty() {
        return AccountState::SignedOut.into();
    }
    AccountState::Unknown.into()
}

/// The text after `label` up to the next whitespace, if whitespace follows.
fn field_after<'a>(text: &'a str, label: &str) -> impl Iterator<Item = &'a str> {
    text.match_indices(label).filter_map(move |(at, _)| {
        let rest = &text[at + label.len()..];
        let end = rest.find(char::is_whitespace)?;
        (end > 0).then(|| &rest[..end])
    })
}

fn device_code(text: &str) -> Option<&str> {
    field_after(text, "one-time code: ").find(|code| {
        let bytes = code.as_bytes();
        bytes.len() == 9
            && bytes[4] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(i, b)| i == 4 || b.is_ascii_uppercase() || b.is_ascii_digit())
    })
}

/// Parses complete lines only, so a partially received URL is never judged.
fn challenge(output: &str, now: u64, timeout: Duration) -> Result<Option<BrowserAction>> {
    let complete = &output[..output.rfind('\n').map_or(0, |end| end + 1)];
    let Some(url) = field_after(complete, "Open this URL to continue in your web browser: ").next()
    else {
        return Ok(None);
    };
    if url != DEVICE_URL {
        // Never turn an arbitrary guest-supplied URL into a credential page.
        return fail(
            "Unexpected GitHub authorization page. Check the installed GitHub CLI version.",
        );
    }
    // gh prints the code line before the URL line; a URL alone means a changed format.
    let Some(code) = device_code(complete) else {
        return fail(
            "GitHub sign-in code was not recognized. Check the installed GitHub CLI version.",
        );
    };
    Ok(Some(BrowserAction {
        url: DEVICE_URL.into(),
        code: code.into(),
        expires_at: now + timeout.as_secs(),
        setup_action: ApplicationSetupAction::SignInGitHub,
        input_required: false,
    }))
}

fn git_value(ctx: &Ctx, git: &Path, key: &str) -> Result<Option<String>> {
    let (code, out) = run(ctx, git, &["config", "--get", key], Duration::from_secs(15))?;
    Ok((code == Some(0)).then(|| out.trim().to_owned()))
}

/// Fills an unset Git author from the account and never overwrites the
/// owner's own values. Values Homecloud wrote earlier, still unchanged,
/// follow a later account change. Best effort: a network or profile problem
/// leaves Git configuration untouched.
fn configure_identity(ctx: &Ctx, login: &str) -> Result<()> {
    let Some(git) = ctx.which("git") else {
        return Ok(());
    };
    let profile = match gh(ctx, &["api", "user"], Duration::from_secs(20)) {
        Ok((Some(0), out)) => serde_json::from_str::<Value>(&out).ok(),
        _ => None,
    };
    let Some(profile) = profile.filter(Value::is_object) else {
        return Ok(());
    };
    let Some(id) = profile.get("id").and_then(Value::as_u64) else {
        return Ok(());
    };
    if profile.get("login").and_then(Value::as_str) != Some(login) {
        return Ok(());
    }
    let desired = [
        (
            "user.name",
            display_text(profile.get("name"), 120).unwrap_or_else(|| login.to_owned()),
        ),
        // GitHub's private address avoids exposing, or being blocked by, a hidden email.
        (
            "user.email",
            format!("{id}+{login}@users.noreply.github.com"),
        ),
    ];
    let marker = identity_path(ctx);
    let previous: BTreeMap<String, Value> = read_json(&marker).unwrap_or_default();
    let mut written = BTreeMap::new();
    for (key, value) in desired {
        let Ok(current) = git_value(ctx, &git, key) else {
            break;
        };
        let ours = current
            .as_deref()
            .is_none_or(|current| previous.get(key).and_then(Value::as_str) == Some(current));
        if !ours {
            continue;
        }
        match run(
            ctx,
            &git,
            &["config", "--global", key, &value],
            Duration::from_secs(15),
        ) {
            Ok((Some(0), _)) => {
                written.insert(key, value);
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    save_json(&marker, &written)
}

pub fn authorize(ctx: &Ctx, job: &mut Job) -> Result<()> {
    match account_info(ctx).state {
        AccountState::SignedIn => return Ok(()), // Never replace an existing account.
        AccountState::SignedOut => {}
        _ => {
            return fail(
                "Could not inspect the existing GitHub sign-in. Refresh its status before signing in.",
            );
        }
    }
    let Some(binary) = ctx.which("gh") else {
        return fail("Install the GitHub CLI before signing in.");
    };
    // Point Git at gh first: a configuration failure leaves no token behind.
    let (code, _) = gh(
        ctx,
        &["auth", "setup-git", "--hostname", HOST, "--force"],
        Duration::from_secs(30),
    )?;
    if code != Some(0) {
        return fail("Could not configure Git to use the GitHub CLI. Try again.");
    }
    // Without a terminal gh prints the device code and URL, never prompting.
    let (mut reader, writer) = io::pipe()?;
    let mut login = command(ctx, &binary);
    login
        .args([
            "auth",
            "login",
            "--hostname",
            HOST,
            "--web",
            "--git-protocol",
            "https",
        ])
        .args(["--skip-ssh-key", "--insecure-storage"])
        .stdout(writer.try_clone()?)
        .stderr(writer);
    process::new_session(&mut login);
    let mut child = Guard::new(login.spawn()?, Duration::from_secs(3));
    drop(login); // Close our copies of the write end so EOF arrives.
    let deadline = Instant::now() + ctx.login_timeout;
    let mut output = String::new();
    let mut published = false;
    let mut buffer = [0u8; 8192];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return fail(EXPIRED);
        }
        if !process::ready(&[reader.as_fd()], remaining.min(Duration::from_secs(1)))?[0] {
            continue;
        }
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        if count == 0 {
            break;
        }
        output.push_str(&String::from_utf8_lossy(&buffer[..count]));
        trim_front(&mut output, MAX_OUTPUT);
        if !published && let Some(action) = challenge(&output, crate::now(), ctx.login_timeout)? {
            job.awaiting(action)?;
            published = true;
        }
    }
    let wait = deadline.max(Instant::now() + Duration::from_secs(1));
    let Some(status) = child.wait_until(wait)? else {
        return fail(EXPIRED);
    };
    // Upstream output can contain identities; only authored messages leave here.
    if !status.success() {
        return fail(FAILURE);
    }
    drop(child);
    job.verifying()?;
    let info = account_info(ctx);
    let (AccountState::SignedIn, Some(login)) = (info.state, info.username) else {
        return fail("Could not confirm the saved GitHub sign-in. Try again.");
    };
    // The sign-in itself succeeded; the author is a convenience.
    let _ = configure_identity(ctx, &login);
    Ok(())
}

/// Removes every saved github.com account from this VM. GitHub keeps the token valid.
pub fn sign_out(ctx: &Ctx) -> Result<()> {
    for _ in 0..8 {
        let info = account_info(ctx);
        let login = match (info.state, info.username) {
            (AccountState::SignedOut, _) => return Ok(()),
            (AccountState::SignedIn, Some(login)) => login,
            _ => {
                return fail(
                    "Could not inspect the GitHub sign-in. Refresh its status and try again.",
                );
            }
        };
        let (code, _) = gh(
            ctx,
            &["auth", "logout", "--hostname", HOST, "--user", &login],
            Duration::from_secs(30),
        )?;
        if code != Some(0) {
            return fail("Could not remove GitHub access from this VM. Try again.");
        }
    }
    fail("Could not remove every GitHub account from this VM. Try again.")
}

/// Keeps the last `limit` bytes, cut at a character boundary.
pub fn trim_front(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut cut = text.len() - limit;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        text.drain(..cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Fixture, read_lines};
    use homecloud_core::ApplicationPhase;

    // State lives in $FAKE_GH/accounts.json (active first); every call is logged.
    const FAKE: &str = r#"import json, os, sys, time
root = os.environ['FAKE_GH']; mode = open(os.path.join(root, 'mode')).read().strip()
state_path = os.path.join(root, 'accounts.json')
accounts = json.load(open(state_path)) if os.path.exists(state_path) else []
args = sys.argv[1:]
with open(os.path.join(root, 'calls'), 'a') as log:
    log.write(json.dumps({'args': args, 'token_env': any(k in os.environ for k in ('GH_TOKEN', 'GITHUB_TOKEN'))}) + '\n')
def out(text):
    sys.stdout.write(text); sys.stdout.flush()
if args[:2] == ['config', 'get']:
    if mode == 'garbled_config': out('not a login!\n'); sys.exit(0)
    if accounts: out(accounts[0] + '\n'); sys.exit(0)
    sys.stderr.write('could not find key "user"\n'); sys.exit(1)
if args[:2] == ['auth', 'setup-git']:
    sys.exit(3 if mode == 'setup_git_fails' else 0)
if args[:2] == ['auth', 'login']:
    out('\n! Failed to copy one-time code to clipboard\n  No clipboard utilities available.\n')
    if mode == 'no_code':
        out('Open this URL to continue in your web browser: https://github.com/login/device\n'); time.sleep(30); sys.exit(1)
    out('! First copy your one-time code: AB12-CD34\n')
    url = 'https://evil.invalid/capture' if mode == 'wrong_url' else 'https://github.com/login/device'
    if mode == 'split_url':
        out('Open this URL to continue in your web browser: https://github.com/lo'); time.sleep(0.3)
        out('gin/device\n')
    else:
        out('Open this URL to continue in your web browser: ' + url + '\n')
    if mode == 'hang': time.sleep(30); sys.exit(1)
    time.sleep(0.2)
    if mode == 'denied':
        sys.stderr.write('PRIVATE-IDENTITY failed https://secret.invalid\n'); sys.exit(1)
    login_as = open(os.path.join(root, 'login_as')).read().strip() if os.path.exists(os.path.join(root, 'login_as')) else 'octo-fixture'
    if mode != 'unsaved':
        json.dump([login_as] + [a for a in accounts if a != login_as], open(state_path, 'w'))
    out('Authentication complete. Logged in as ' + login_as + '\n'); sys.exit(0)
if args[:2] == ['auth', 'logout']:
    user = args[args.index('--user') + 1]
    if mode == 'logout_fails' or user not in accounts: sys.exit(1)
    json.dump([a for a in accounts if a != user], open(state_path, 'w')); sys.exit(0)
if args == ['api', 'user']:
    if mode == 'api_fails': sys.exit(1)
    login = accounts[0] if accounts else None
    out(json.dumps({'login': login, 'id': {'octo-fixture': 4242, 'new-fixture': 99}.get(login, 7), 'name': None if mode == 'no_name' else 'Octo Fixture'}))
    sys.exit(0)
sys.exit(64)
"#;

    struct Gh {
        fx: Fixture,
    }

    impl Gh {
        fn new() -> Self {
            let mut fx = Fixture::new();
            fx.python_tool("gh", FAKE);
            fx.link_system_tool("git");
            fx.ctx.set_var("FAKE_GH", fx.root.path());
            fx.ctx.set_var("GIT_CONFIG_NOSYSTEM", "1");
            fx.ctx.set_var("GH_TOKEN", "PRIVATE-ENV-TOKEN");
            let gh = Self { fx };
            gh.mode("success", None);
            gh
        }

        fn mode(&self, mode: &str, accounts: Option<&[&str]>) {
            self.fx.write("mode", mode);
            if let Some(accounts) = accounts {
                self.fx
                    .write("accounts.json", &serde_json::to_string(accounts).unwrap());
            }
        }

        fn calls(&self) -> Vec<Value> {
            read_lines(&self.fx.root.path().join("calls"))
        }

        fn call_args(&self) -> Vec<Vec<String>> {
            self.calls()
                .iter()
                .map(|c| serde_json::from_value(c["args"].clone()).unwrap())
                .collect()
        }

        fn sign_in(&self) -> (Job, Option<String>) {
            let mut job =
                crate::state::test_job(self.fx.root.path(), ApplicationSetupAction::SignInGitHub);
            let error = authorize(&self.fx.ctx, &mut job)
                .err()
                .map(|e| e.user_message().to_owned());
            (job, error)
        }

        fn git(&self, args: &[&str]) -> Option<String> {
            let git = self.fx.ctx.which("git").unwrap();
            let (code, out) = run(&self.fx.ctx, &git, args, Duration::from_secs(15)).unwrap();
            (code == Some(0)).then(|| out.trim().to_owned())
        }
    }

    fn stages(job: &Job) -> Vec<&str> {
        job.history.iter().map(|s| s.stage.as_str()).collect()
    }

    #[test]
    fn account_state_is_local_and_never_reads_a_token() {
        let gh = Gh::new();
        assert_eq!(account_info(&gh.fx.ctx), AccountState::SignedOut.into());
        gh.mode("success", Some(&["octo-fixture"]));
        assert_eq!(
            account_info(&gh.fx.ctx).username.as_deref(),
            Some("octo-fixture")
        );
        gh.mode("garbled_config", None);
        assert_eq!(account_info(&gh.fx.ctx), AccountState::Unknown.into());
        assert!(
            gh.calls()
                .iter()
                .all(|c| c["args"][0] == "config" && c["token_env"] == false)
        );
        std::fs::remove_file(gh.fx.bin.join("gh")).unwrap();
        assert_eq!(account_info(&gh.fx.ctx), AccountState::Unavailable.into());
    }

    #[test]
    fn device_flow_configures_git_first_and_publishes_only_the_exact_challenge() {
        let gh = Gh::new();
        let (job, error) = gh.sign_in();
        assert_eq!(error, None);
        assert_eq!(stages(&job), ["awaiting_approval", "verifying"]);
        let action = job.history[0].action.as_ref().unwrap();
        assert_eq!(
            (
                action.url.as_str(),
                action.code.as_str(),
                action.setup_action
            ),
            (
                DEVICE_URL,
                "AB12-CD34",
                ApplicationSetupAction::SignInGitHub
            )
        );
        let calls = gh.call_args();
        let position = |cmd: &str| calls.iter().position(|c| c[..2] == ["auth", cmd]).unwrap();
        assert!(position("setup-git") < position("login"));
        let login = &calls[position("login")];
        for flag in ["--web", "--insecure-storage", "--skip-ssh-key"] {
            assert!(login.iter().any(|a| a == flag), "{flag}");
        }
        let protocol = login.iter().position(|a| a == "--git-protocol").unwrap();
        assert_eq!(login[protocol + 1], "https");
        assert!(!login.iter().any(|a| a == "--scopes"));
        assert!(gh.calls().iter().all(|c| c["token_env"] == false));
    }

    #[test]
    fn partial_url_line_is_not_misjudged() {
        let gh = Gh::new();
        gh.mode("split_url", None);
        let (job, error) = gh.sign_in();
        assert_eq!(error, None);
        assert_eq!(job.history[0].action.as_ref().unwrap().url, DEVICE_URL);
    }

    #[test]
    fn existing_account_is_not_replaced_or_prompted() {
        let gh = Gh::new();
        gh.mode("success", Some(&["someone-else"]));
        let (job, error) = gh.sign_in();
        assert_eq!((job.history.len(), error), (0, None));
        assert!(!gh.call_args().iter().any(|c| c[..2] == ["auth", "login"]));
    }

    #[test]
    fn unexpected_page_or_format_is_rejected_without_a_challenge() {
        for (mode, message) in [
            ("wrong_url", "Unexpected GitHub authorization page"),
            ("no_code", "code was not recognized"),
        ] {
            let gh = Gh::new();
            gh.mode(mode, None);
            let (job, error) = gh.sign_in();
            assert!(job.history.is_empty());
            assert!(error.unwrap().contains(message));
        }
    }

    #[test]
    fn git_configuration_failure_stops_before_any_login() {
        let gh = Gh::new();
        gh.mode("setup_git_fails", None);
        let (job, error) = gh.sign_in();
        assert!(job.history.is_empty());
        assert!(error.unwrap().contains("configure Git"));
        assert!(!gh.call_args().iter().any(|c| c[..2] == ["auth", "login"]));
    }

    #[test]
    fn denial_or_unsaved_credentials_discard_upstream_output() {
        for mode in ["denied", "unsaved"] {
            let gh = Gh::new();
            gh.mode(mode, Some(&[]));
            let (job, error) = gh.sign_in();
            let error = error.unwrap();
            assert!(!error.contains("PRIVATE") && !error.contains("secret.invalid"));
            assert!(
                !serde_json::to_string(&job.state)
                    .unwrap()
                    .contains("PRIVATE")
            );
        }
    }

    #[test]
    fn attempt_is_bounded_and_the_cli_is_stopped() {
        let mut gh = Gh::new();
        gh.mode("hang", None);
        gh.fx.ctx.login_timeout = Duration::from_secs(1);
        let started = Instant::now();
        let (job, error) = gh.sign_in();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(job.history.len(), 1);
        assert_eq!(job.state.phase, ApplicationPhase::WaitingForUser);
        assert!(error.unwrap().contains("expired"));
    }

    #[test]
    fn unset_git_author_uses_the_private_noreply_address() {
        let gh = Gh::new();
        assert_eq!(gh.sign_in().1, None);
        assert_eq!(
            gh.git(&["config", "--global", "user.name"]).as_deref(),
            Some("Octo Fixture")
        );
        assert_eq!(
            gh.git(&["config", "--global", "user.email"]).as_deref(),
            Some("4242+octo-fixture@users.noreply.github.com")
        );
    }

    #[test]
    fn owner_git_author_is_never_overwritten() {
        let gh = Gh::new();
        gh.git(&["config", "--global", "user.name", "Owner Chosen"]);
        assert_eq!(gh.sign_in().1, None);
        assert_eq!(
            gh.git(&["config", "--global", "user.name"]).as_deref(),
            Some("Owner Chosen")
        );
        assert_eq!(
            gh.git(&["config", "--global", "user.email"]).as_deref(),
            Some("4242+octo-fixture@users.noreply.github.com")
        );
    }

    #[test]
    fn homecloud_author_follows_a_new_account_until_the_owner_changes_it() {
        let gh = Gh::new();
        assert_eq!(gh.sign_in().1, None);
        gh.git(&["config", "--global", "user.name", "Edited By Owner"]);
        sign_out(&gh.fx.ctx).unwrap();
        gh.fx.write("login_as", "new-fixture");
        assert_eq!(gh.sign_in().1, None);
        assert_eq!(
            gh.git(&["config", "--global", "user.name"]).as_deref(),
            Some("Edited By Owner")
        );
        assert_eq!(
            gh.git(&["config", "--global", "user.email"]).as_deref(),
            Some("99+new-fixture@users.noreply.github.com")
        );
    }

    #[test]
    fn identity_lookup_failure_does_not_fail_sign_in() {
        let gh = Gh::new();
        gh.mode("api_fails", None);
        assert_eq!(gh.sign_in().1, None);
        assert_eq!(gh.git(&["config", "--global", "user.email"]), None);
        gh.mode("no_name", Some(&[]));
        assert_eq!(gh.sign_in().1, None);
        assert_eq!(
            gh.git(&["config", "--global", "user.name"]).as_deref(),
            Some("octo-fixture")
        );
    }

    #[test]
    fn sign_out_removes_every_saved_account_and_is_idempotent() {
        let gh = Gh::new();
        gh.mode("success", Some(&["octo-fixture", "second-fixture"]));
        sign_out(&gh.fx.ctx).unwrap();
        assert_eq!(account_info(&gh.fx.ctx), AccountState::SignedOut.into());
        sign_out(&gh.fx.ctx).unwrap();
        gh.mode("logout_fails", Some(&["octo-fixture"]));
        let error = sign_out(&gh.fx.ctx).unwrap_err();
        assert!(
            error
                .user_message()
                .contains("Could not remove GitHub access")
        );
    }

    #[test]
    fn challenge_parser_accepts_only_the_device_page_and_code_shape() {
        let text = "! First copy your one-time code: AB12-CD34\nOpen this URL to continue in your web browser: https://github.com/login/device\n";
        let action = challenge(text, 1000, Duration::from_secs(600))
            .unwrap()
            .unwrap();
        assert_eq!(
            (action.code.as_str(), action.expires_at),
            ("AB12-CD34", 1600)
        );
        assert!(
            challenge(&text[..text.len() - 1], 1000, Duration::from_secs(600))
                .unwrap()
                .is_none()
        );
        let lower = text.replace("AB12", "ab12");
        assert!(challenge(&lower, 1000, Duration::from_secs(600)).is_err());
    }
}
