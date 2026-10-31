//! Version-pinned Claude CLI browser login, supervised in the guest.
//!
//! OAuth and credential storage belong to Claude. Only an allowlisted URL and
//! normalized account metadata leave this module; the code the browser
//! returns crosses a FIFO to the same login process.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use homecloud_core::{ApplicationPhase, ApplicationSetupAction, BrowserAction};
use nix::libc;
use nix::pty::{Winsize, openpty};
use nix::sys::stat::Mode;
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use serde_json::Value;
use uuid::Uuid;

use crate::ctx::Ctx;
use crate::error::{Result, fail};
use crate::fsutil::{read_json, try_lock};
use crate::github::trim_front;
use crate::metadata::{AccountInfo, AccountState, claude_account};
use crate::process::{self, Guard};
use crate::state::{Job, JobState};
use crate::text::strip_csi;

const MAX_OUTPUT: usize = 65536;
const UNAVAILABLE: &str = "Claude sign-in input is unavailable. Start again.";

pub fn account_info(ctx: &Ctx) -> AccountInfo {
    let Some(binary) = ctx.which("claude") else {
        return AccountState::Unavailable.into();
    };
    let mut command = ctx.command(binary);
    command.args(["auth", "status"]);
    match process::capture(command, Duration::from_secs(15), MAX_OUTPUT + 1) {
        Ok(Some(output)) if output.stdout.len() <= MAX_OUTPUT => {
            match serde_json::from_slice::<Value>(&output.stdout) {
                Ok(value) => claude_account(&value, output.code),
                Err(_) => AccountState::Unknown.into(),
            }
        }
        _ => AccountState::Unknown.into(),
    }
}

/// Every candidate URL in terminal output: visible URLs first, then OSC 8
/// hyperlink targets.
pub fn terminal_urls(output: &str) -> Vec<String> {
    let chars: Vec<char> = output.chars().collect();
    let mut targets = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\x1b' && chars.get(i + 1) == Some(&']') {
            let body_end = (i + 2..chars.len())
                .find(|j| chars[*j] == '\x07' || chars[*j] == '\x1b')
                .unwrap_or(chars.len());
            let terminator = match (chars.get(body_end), chars.get(body_end + 1)) {
                (Some('\x07'), _) => Some(1),
                (Some('\x1b'), Some('\\')) => Some(2),
                _ => None,
            };
            if let Some(length) = terminator {
                // Remove complete frames as units; stripping just ESC joins target and label.
                let body: String = chars[i + 2..body_end].iter().collect();
                let fields: Vec<&str> = body.splitn(3, ';').collect();
                if fields.len() == 3 && fields[0] == "8" && !fields[2].is_empty() {
                    targets.push(fields[2].to_owned());
                }
                i = body_end + length;
                continue;
            }
            if body_end == chars.len() {
                // A partial frame may be completed by the next read.
                break;
            }
        }
        plain.push(chars[i]);
        i += 1;
    }
    let plain = strip_csi(&plain);
    let mut urls = Vec::new();
    let mut rest = plain.as_str();
    while let Some(at) = rest.find("https://") {
        let tail = &rest[at + "https://".len()..];
        let length = tail
            .find(|c: char| {
                c.is_whitespace() || (c as u32) <= 0x20 || (0x7f..=0x9f).contains(&(c as u32))
            })
            .unwrap_or(tail.len());
        if length > 0 {
            urls.push(rest[at..at + "https://".len() + length].to_owned());
        }
        rest = &tail[length..];
    }
    urls.extend(targets);
    urls
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|digit| digit as u8)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| Some(hex(*bytes.get(i + 1)?)? * 16 + hex(*bytes.get(i + 2)?)?))
            .flatten();
        match (bytes[i], escaped) {
            (_, Some(byte)) => {
                out.push(byte);
                i += 3;
                continue;
            }
            (b'+', None) => out.push(b' '),
            (byte, None) => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Query values by name, like Python's `parse_qs`: blank values are dropped.
fn query_values<'a>(query: &'a str, name: &'a str) -> impl Iterator<Item = String> + 'a {
    query.split('&').filter_map(move |pair| {
        let (key, value) = pair.split_once('=')?;
        (!value.is_empty() && percent_decode(key) == name).then(|| percent_decode(value))
    })
}

fn accepted(url: &str) -> bool {
    if url
        .chars()
        .any(|c| (c as u32) < 32 || (127..=159).contains(&(c as u32)))
    {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let rest = rest.split('#').next().unwrap_or_default();
    let (location, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = location.split_at(location.find('/').unwrap_or(location.len()));
    let only = |name, expected: &str| {
        let values: Vec<String> = query_values(query, name).collect();
        !values.is_empty() && values.iter().all(|value| value == expected)
    };
    let present = |name| query_values(query, name).next().is_some();
    host == "claude.com"
        && path == "/cai/oauth/authorize"
        && only("response_type", "code")
        && only("code", "true")
        && present("state")
        && present("code_challenge")
}

pub fn challenge(output: &str, now: u64, timeout: Duration) -> Option<BrowserAction> {
    terminal_urls(output)
        .into_iter()
        .find(|url| accepted(url))
        .map(|url| BrowserAction {
            url,
            code: String::new(),
            expires_at: now + timeout.as_secs(),
            setup_action: ApplicationSetupAction::SignInClaude,
            input_required: true,
        })
}

pub fn input_code(value: Option<&str>) -> Result<String> {
    match value {
        Some(code)
            if (8..=2048).contains(&code.len())
                && code
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.~#/+%=:?-".contains(&b)) =>
        {
            Ok(code.to_owned())
        }
        _ => fail("Paste the authorization code shown by Claude, then try again."),
    }
}

fn fifo_path(directory: &Path, id: Uuid) -> PathBuf {
    directory.join(format!("{id}.input"))
}

fn waiting_for_code(state: &JobState, id: Uuid, boot_id: &str) -> bool {
    state.id == id
        && state.setup_action == ApplicationSetupAction::SignInClaude
        && state.phase == ApplicationPhase::WaitingForUser
        && state.boot_id == boot_id
        && state
            .action
            .as_ref()
            .is_some_and(|action| action.input_required && action.expires_at > crate::now())
}

/// Hands the browser's code to the waiting login process.
pub fn submit(ctx: &Ctx, directory: &Path, id: Uuid, value: Option<&str>) -> Result<()> {
    let code = input_code(value)?;
    let state: Option<JobState> = read_json(&directory.join("job.json"));
    if !state.is_some_and(|state| waiting_for_code(&state, id, &ctx.boot_id().unwrap_or_default()))
    {
        return fail("This Claude sign-in attempt is no longer waiting for a code. Start again.");
    }
    let Ok(Some(_lock)) = try_lock(&directory.join("input.lock")) else {
        return fail(UNAVAILABLE);
    };
    let Ok(mut fifo) = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(fifo_path(directory, id))
    else {
        return fail(UNAVAILABLE);
    };
    let owned = fifo.metadata().is_ok_and(|meta| {
        meta.file_type().is_fifo() && meta.uid() == nix::unistd::getuid().as_raw()
    });
    if !owned {
        return fail(UNAVAILABLE);
    }
    let line = format!("{code}\n");
    match fifo.write(line.as_bytes()) {
        Ok(count) if count == line.len() => {}
        Ok(_) => return fail("Could not deliver the authorization code. Try again."),
        Err(_) => return fail(UNAVAILABLE),
    }
    drop(fifo);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let latest: Option<JobState> = read_json(&directory.join("job.json"));
        if latest.is_some_and(|latest| {
            latest.id == id && latest.phase != ApplicationPhase::WaitingForUser
        }) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    fail("Code sent; confirmation is pending. Refresh the sign-in status.")
}

/// Removes the input FIFO however the attempt ends.
struct Fifo(PathBuf, File);

impl Drop for Fifo {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn authorize(ctx: &Ctx, job: &mut Job) -> Result<()> {
    match account_info(ctx).state {
        AccountState::SignedIn => return Ok(()),
        AccountState::SignedOut | AccountState::Unavailable => {}
        AccountState::Unknown => {
            return fail(
                "Could not inspect the existing Claude account. Refresh its status before signing in.",
            );
        }
    }
    let Some(binary) = ctx.which("claude") else {
        return fail("Install Claude Code before signing in.");
    };
    let path = fifo_path(job.directory(), job.state.id);
    nix::unistd::mkfifo(&path, Mode::from_bits_truncate(0o600))?;
    // Holding a writer as well keeps reads from ever seeing end-of-file.
    let opened = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(&path);
    let mut fifo = match opened {
        Ok(file) => Fifo(path, file),
        Err(error) => {
            let _ = fs::remove_file(&path);
            return Err(error.into());
        }
    };
    // Keep the long OAuth URL on one line and don't echo the returned code.
    let size = Winsize {
        ws_row: 40,
        ws_col: 4096,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = openpty(Some(&size), None)?;
    let mut attrs = tcgetattr(&pty.slave)?;
    attrs.local_flags.remove(LocalFlags::ECHO);
    tcsetattr(&pty.slave, SetArg::TCSANOW, &attrs)?;
    let mut master = File::from(pty.master);
    let mut command = ctx.command(binary);
    command
        .args(["auth", "login", "--claudeai"])
        .env("BROWSER", "/usr/bin/true")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(pty.slave.try_clone()?))
        .stdout(Stdio::from(pty.slave.try_clone()?))
        .stderr(Stdio::from(pty.slave));
    process::new_session(&mut command);
    let mut child = Guard::new(command.spawn()?, Duration::from_secs(3));
    drop(command);

    let deadline = Instant::now() + ctx.login_timeout;
    let mut output = String::new();
    let mut action: Option<BrowserAction> = None;
    let mut delivered = false;
    let mut pending: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 8192];
    while Instant::now() < deadline {
        let ready = process::ready(
            &[master.as_fd(), fifo.1.as_fd()],
            Duration::from_millis(200),
        )?;
        if ready[0] {
            match master.read(&mut buffer) {
                Ok(count) if count > 0 => {
                    output.push_str(&String::from_utf8_lossy(&buffer[..count]));
                    trim_front(&mut output, MAX_OUTPUT);
                }
                // The terminal closes when the CLI exits; its status is checked below.
                _ => thread::sleep(Duration::from_millis(20)),
            }
            if action.is_none() {
                let plain = strip_csi(&output);
                let prompt = plain.contains("Paste code");
                action = challenge(&output, crate::now(), ctx.login_timeout).filter(|_| prompt);
                if action.is_none() && prompt && plain.contains("https://") {
                    return fail(
                        "Claude authorization page format was not recognized. Check the installed CLI version.",
                    );
                }
                if let Some(action) = &action {
                    job.awaiting(action.clone())?;
                }
            }
        }
        if ready[1] {
            let count = match fifo.1.read(&mut buffer[..4096]) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => 0,
                Err(error) => return Err(error.into()),
            };
            pending.extend_from_slice(&buffer[..count]);
            if pending.len() > 4096 {
                return fail("Invalid Claude authorization response. Start again.");
            }
        }
        if !delivered && let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line = String::from_utf8(pending[..end].to_vec()).ok();
            let code = input_code(line.as_deref())?;
            // Don't send input before the pinned CLI is expecting its code.
            if !strip_csi(&output).contains("Paste code") {
                return fail("Claude is not ready for the authorization code. Start again.");
            }
            delivered = true;
            pending.clear();
            output.clear();
            job.verifying()?;
            master.write_all(format!("{code}\r").as_bytes())?;
        }
        if let Some(status) = child.exited()? {
            if status.success() && account_info(ctx).state == AccountState::SignedIn {
                return Ok(());
            }
            return fail(
                "Claude sign-in did not finish. Start a new sign-in attempt to try again.",
            );
        }
        if action
            .as_ref()
            .is_some_and(|action| crate::now() >= action.expires_at)
        {
            break;
        }
    }
    fail("Claude sign-in attempt expired. Start again for a new link.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;

    const URL: &str = "https://claude.com/cai/oauth/authorize?code=true&response_type=code&state=DEMO-STATE&code_challenge=DEMO-CHALLENGE";
    const FAKE: &str = r#"
import json,sys
from pathlib import Path
marker=Path(MARKER)
if sys.argv[1:]==['auth','status']:
 print(json.dumps({'loggedIn':marker.exists(),'email':'PRIVATE-IDENTITY'}));sys.exit(0 if marker.exists() else 1)
print(URL,flush=True)
print('Paste code here if prompted >',flush=True)
code=sys.stdin.readline().strip()
if code!='FIXTURE-RETURN-CODE':sys.exit(1)
marker.write_text('saved')
"#;

    #[test]
    fn url_parser_is_specific_and_does_not_accept_other_destinations() {
        let action = challenge(&format!("\x1b[32m{URL}"), 1000, Duration::from_secs(600)).unwrap();
        assert_eq!(action.expires_at, 1600);
        assert!(action.input_required);
        assert_eq!(action.setup_action, ApplicationSetupAction::SignInClaude);
        let timeout = Duration::from_secs(600);
        assert!(challenge(&format!("{URL}&response_type=code"), 1000, timeout).is_some());
        assert!(challenge(&format!("{URL}&response_type=token"), 1000, timeout).is_none());
        assert!(challenge(&format!("{URL}&state="), 1000, timeout).is_some());
        for url in [
            URL.replace("claude.com", "evil.invalid"),
            URL.replace("claude.com", "claude.com.evil.invalid"),
            URL.replace("response_type=code", "response_type=token"),
            URL.replace("https:", "http:"),
            URL.replace("/cai/oauth/authorize", "/wrong"),
            URL.replace("&state=DEMO-STATE", "&state="),
        ] {
            assert!(challenge(&url, 1000, timeout).is_none(), "{url}");
        }
    }

    #[test]
    fn terminal_hyperlink_fixtures_match_the_desktop_browser_contract() {
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/claude-auth-terminal.json")).unwrap();
        assert!(!fixtures.is_empty());
        for fixture in fixtures {
            let result = challenge(
                fixture["terminal"].as_str().unwrap(),
                1000,
                Duration::from_secs(600),
            );
            let url = result.as_ref().map(|action| action.url.as_str());
            assert_eq!(url, fixture["url"].as_str(), "{}", fixture["name"]);
            if let Some(url) = url {
                assert!(url.starts_with("https://"));
                assert!(
                    !url.chars()
                        .any(|c| (c as u32) < 32 || (127..=159).contains(&(c as u32)))
                );
                assert_eq!(url.matches("https://").count(), 1);
            }
        }
    }

    #[test]
    fn input_rejects_controls_newlines_and_oversized_values() {
        let long = "x".repeat(2049);
        let smiles = "☺".repeat(10);
        for value in [
            Some("bad"),
            Some("CODE\nANOTHER"),
            Some(long.as_str()),
            None,
            Some(smiles.as_str()),
        ] {
            assert!(input_code(value).is_err());
        }
        assert_eq!(
            input_code(Some("code_part#state-part")).unwrap(),
            "code_part#state-part"
        );
    }

    #[derive(Default)]
    struct Flow {
        invalid: bool,
        already: bool,
        expire: bool,
        bad_url: bool,
        hyperlink: bool,
    }

    struct Outcome {
        history: Vec<JobState>,
        error: Option<String>,
        saved: Option<String>,
    }

    fn run_flow(flow: Flow) -> Outcome {
        let mut fx = Fixture::new();
        let marker = fx.root.path().join("authenticated");
        let url = if flow.bad_url {
            URL.replace("/cai/oauth/authorize", "/unsupported")
        } else {
            URL.into()
        };
        let terminal = if flow.hyperlink {
            format!("\x1b]8;;{url}\x07{url}\x1b]8;;\x07")
        } else {
            url
        };
        let script = FAKE
            .replace("MARKER", &serde_json::to_string(&marker).unwrap())
            .replace("URL", &serde_json::to_string(&terminal).unwrap());
        fx.python_tool("claude", &script);
        if flow.already {
            fs::write(&marker, "existing").unwrap();
        }
        fx.write("boot", "fixture-boot");
        if flow.expire {
            fx.ctx.login_timeout = Duration::from_secs(1);
        }
        let ctx = fx.ctx.clone();
        let directory = fx.root.path().to_path_buf();
        let worker = {
            let ctx = ctx.clone();
            let directory = directory.clone();
            thread::spawn(move || {
                let mut job =
                    crate::state::test_job(&directory, ApplicationSetupAction::SignInClaude);
                let error = authorize(&ctx, &mut job)
                    .err()
                    .map(|e| e.user_message().to_owned());
                (job.history, error)
            })
        };
        let path = directory.join("job.json");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !worker.is_finished() {
            let state: Option<JobState> = read_json(&path);
            if state.is_some_and(|state| state.phase == ApplicationPhase::WaitingForUser) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if !flow.already && !flow.expire && !flow.bad_url {
            let state: JobState = read_json(&path).expect("login fixture published readiness");
            assert_eq!(state.phase, ApplicationPhase::WaitingForUser);
            let code = if flow.invalid {
                "WRONG-RETURN-CODE"
            } else {
                "FIXTURE-RETURN-CODE"
            };
            let _ = submit(&ctx, &directory, Uuid::from_u128(2), Some(code));
        }
        let (history, error) = worker.join().unwrap();
        assert!(!fifo_path(&directory, Uuid::from_u128(2)).exists());
        let published = serde_json::to_string(&history).unwrap();
        assert!(
            !published.contains("FIXTURE-RETURN-CODE") && !published.contains("PRIVATE-IDENTITY")
        );
        Outcome {
            history,
            error,
            saved: fs::read_to_string(&marker).ok(),
        }
    }

    #[test]
    fn browser_code_reaches_same_process_without_becoming_persistent_state() {
        let outcome = run_flow(Flow::default());
        assert_eq!(outcome.error, None);
        assert_eq!(outcome.saved.as_deref(), Some("saved"));
        let stages: Vec<&str> = outcome.history.iter().map(|s| s.stage.as_str()).collect();
        assert_eq!(stages, ["awaiting_approval", "verifying"]);
        assert!(outcome.history[1].action.is_none());
    }

    #[test]
    fn hyperlinked_cli_url_and_return_code_complete_the_same_process() {
        let outcome = run_flow(Flow {
            hyperlink: true,
            ..Flow::default()
        });
        assert_eq!(outcome.error, None);
        assert_eq!(outcome.saved.as_deref(), Some("saved"));
        assert_eq!(outcome.history[0].action.as_ref().unwrap().url, URL);
    }

    #[test]
    fn existing_account_is_preserved_without_starting_login() {
        let outcome = run_flow(Flow {
            already: true,
            ..Flow::default()
        });
        assert!(outcome.history.is_empty() && outcome.error.is_none());
        assert_eq!(outcome.saved.as_deref(), Some("existing"));
    }

    #[test]
    fn bad_code_and_expiry_finish_with_retryable_error() {
        for flow in [
            Flow {
                invalid: true,
                ..Flow::default()
            },
            Flow {
                expire: true,
                ..Flow::default()
            },
        ] {
            let outcome = run_flow(flow);
            let error = outcome.error.unwrap();
            assert!(outcome.saved.is_none());
            assert!(!error.contains("WRONG-RETURN-CODE"));
        }
    }

    #[test]
    fn unsupported_url_fails_explicitly_instead_of_waiting_silently() {
        let outcome = run_flow(Flow {
            bad_url: true,
            ..Flow::default()
        });
        assert!(outcome.saved.is_none());
        assert!(outcome.error.unwrap().contains("format was not recognized"));
    }

    #[test]
    fn submission_rejects_wrong_or_finished_job_and_regular_file() {
        let fx = Fixture::new();
        fx.write("boot", "fixture-boot");
        let directory = fx.root.path();
        let mut job = crate::state::test_job(directory, ApplicationSetupAction::SignInClaude);
        job.state.phase = ApplicationPhase::WaitingForUser;
        job.state.action = Some(BrowserAction {
            url: URL.into(),
            code: String::new(),
            expires_at: crate::now() + 30,
            setup_action: ApplicationSetupAction::SignInClaude,
            input_required: true,
        });
        job.save().unwrap();
        let id = job.state.id;
        assert!(submit(&fx.ctx, directory, Uuid::from_u128(9), Some("FIXTURE-CODE")).is_err());
        job.state.phase = ApplicationPhase::Succeeded;
        job.save().unwrap();
        assert!(submit(&fx.ctx, directory, id, Some("FIXTURE-CODE")).is_err());
        job.state.phase = ApplicationPhase::WaitingForUser;
        job.save().unwrap();
        fs::write(fifo_path(directory, id), "existing").unwrap();
        let error = submit(&fx.ctx, directory, id, Some("FIXTURE-CODE")).unwrap_err();
        assert_eq!(error.user_message(), UNAVAILABLE);
        assert_eq!(
            fs::read_to_string(fifo_path(directory, id)).unwrap(),
            "existing"
        );
    }
}
