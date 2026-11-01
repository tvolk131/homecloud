//! T3's own CLI: connection status, version, and the headless account link.
//! OAuth device secrets stay with T3's CLI process and its secret store.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use homecloud_core::BrowserAction;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ctx::Ctx;
use crate::error::{Result, fail};
use crate::fsutil::{mtime_ns, read_json, save_json};
use crate::github::trim_front;
use crate::process::{self, Guard};
use crate::state::Job;
use crate::text::{find_version, strip_csi};

/// `t3 connect status --json`, read leniently.
pub struct Status(Map<String, Value>);

impl Status {
    fn flag(&self, key: &str) -> bool {
        self.0.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    pub fn revoked(&self) -> bool {
        self.flag("revoked")
    }
    pub fn authenticated(&self) -> bool {
        self.flag("authenticated")
    }
    /// Account authorized, connection wanted and linked.
    pub fn linked(&self) -> bool {
        self.authenticated() && self.flag("desired") && self.flag("linked")
    }
    pub fn reachable(&self) -> bool {
        self.0
            .get("reachability")
            .and_then(|r| r.get("status"))
            .and_then(Value::as_str)
            == Some("reachable")
    }
    pub fn cloud_user_id(&self) -> Option<&Value> {
        self.0.get("cloudUserId")
    }
}

pub fn status(ctx: &Ctx) -> Option<Status> {
    if !ctx.t3().exists() {
        return None;
    }
    let mut command = ctx.command(ctx.t3());
    command.args(["connect", "status", "--json"]);
    let output = process::capture(command, Duration::from_secs(15), 1024 * 1024).ok()??;
    if !output.success() {
        return None;
    }
    match serde_json::from_slice(&output.stdout) {
        Ok(Value::Object(map)) => Some(Status(map)),
        _ => None,
    }
}

#[derive(Serialize, Deserialize)]
struct VersionCache {
    stamp: (i64, u64),
    version: String,
}

/// The installed T3 version, cached until the binary changes.
pub fn version(ctx: &Ctx, directory: &Path) -> Option<String> {
    let meta = std::fs::metadata(ctx.t3()).ok()?;
    let stamp = (mtime_ns(&meta), meta.size());
    let path = directory.join("version.json");
    if let Some(cache) = read_json::<VersionCache>(&path).filter(|cache| cache.stamp == stamp) {
        return Some(cache.version);
    }
    let mut command = ctx.command(ctx.t3());
    command.arg("--version");
    let output = process::capture(command, Duration::from_secs(30), 65536).ok()??;
    if !output.success() {
        return None;
    }
    let version = find_version(&output.text(), true)?;
    let _ = save_json(
        &path,
        &VersionCache {
            stamp,
            version: version.clone(),
        },
    );
    Some(version)
}

/// Text after `label`, skipping whitespace, up to the next whitespace.
fn after<'a>(text: &'a str, label: &str) -> impl Iterator<Item = &'a str> {
    text.match_indices(label).filter_map(move |(at, _)| {
        let rest = text[at + label.len()..].trim_start();
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        (end > 0).then(|| &rest[..end])
    })
}

/// The headless link challenge, once T3 has printed all of it.
pub fn parse_challenge(output: &str, now: u64) -> Option<BrowserAction> {
    let text = strip_csi(output).replace('\r', "");
    if !text.contains("Headless authorization") {
        return None;
    }
    let url = after(&text, "Open this URL on a device with a browser:")
        .find(|url| url.starts_with("https://") && url.len() > "https://".len())?;
    let code = after(&text, "Confirm this code when asked:").next()?;
    let minutes = text.match_indices("expires in ").find_map(|(at, label)| {
        let rest = &text[at + label.len()..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && rest[digits..].starts_with(" min"))
            .then(|| rest[..digits].parse::<u64>().unwrap_or(u64::MAX))
    })?;
    // The CLI rounds minutes; expire early rather than advertise a stale code.
    let ttl = minutes
        .saturating_mul(60)
        .saturating_sub(30)
        .clamp(30, 1800);
    Some(BrowserAction {
        url: url.to_owned(),
        code: code.to_owned(),
        expires_at: now + ttl,
        setup_action: Default::default(),
        input_required: false,
    })
}

/// Runs `t3 connect link --headless` on a terminal until the owner approves.
pub fn link(ctx: &Ctx, job: &mut Job) -> Result<()> {
    let pty = process::terminal(None)?;
    let mut master = File::from(pty.master);
    let mut command = ctx.command(ctx.t3());
    command
        .args(["connect", "link", "--headless"])
        .stdin(Stdio::from(pty.slave.try_clone()?))
        .stdout(Stdio::from(pty.slave.try_clone()?))
        .stderr(Stdio::from(pty.slave));
    process::new_session(&mut command);
    let mut child = Guard::new(command.spawn()?, Duration::from_secs(5));
    drop(command);
    let deadline = Instant::now() + Duration::from_secs(1800);
    let mut output = String::new();
    let mut answered = false;
    let mut challenge: Option<u64> = None;
    let mut buffer = [0u8; 8192];
    while Instant::now() < deadline {
        if process::ready(&[master.as_fd()], Duration::from_millis(200))?[0] {
            match master.read(&mut buffer) {
                Ok(count) if count > 0 => {
                    output.push_str(&String::from_utf8_lossy(&buffer[..count]));
                    trim_front(&mut output, 65536);
                }
                _ => thread::sleep(Duration::from_millis(20)),
            }
            let plain = strip_csi(&output);
            // Only approve T3's known, version-pinned relay installation prompt.
            if !answered
                && plain.contains("Download and install version")
                && plain.contains("T3 relay client")
            {
                master.write_all(b"y\r")?;
                answered = true;
            }
            if challenge.is_none()
                && let Some(mut action) = parse_challenge(&output, crate::now())
            {
                action.setup_action = job.state.setup_action;
                challenge = Some(action.expires_at);
                job.awaiting(action)?;
            }
        }
        if let Some(status) = child.exited()? {
            if !status.success() {
                return fail("Authorization did not finish. Request a new link to try again.");
            }
            return Ok(());
        }
        if challenge.is_some_and(|expires_at| crate::now() >= expires_at) {
            return fail("Authorization link expired. Request a new link to continue.");
        }
    }
    fail("T3 setup timed out. Try connecting again.")
}

/// Links this VM to the owner's T3 account unless it already is.
pub fn connect(ctx: &Ctx, job: &mut Job) -> Result<()> {
    if status(ctx).is_some_and(|status| status.linked() && !status.revoked()) {
        return Ok(());
    }
    link(ctx, job)?;
    job.verifying()?;
    let mut restart = ctx.command(ctx.t3());
    restart.args(["service", "restart"]);
    if !process::capture(restart, Duration::from_secs(60), 65536)?
        .is_some_and(|output| output.success())
    {
        return fail("Could not restart T3 Code after connecting. Try again.");
    }
    // Saved credentials survive network trouble; pairing need not be repeated.
    if !status(ctx).is_some_and(|status| status.authenticated() && status.flag("desired")) {
        return fail("Could not confirm saved account connection. Inspect T3 and try again.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_parser_is_specific_and_has_a_bounded_lifetime() {
        let text = "\x1b[32mHeadless authorization\nOpen this URL on a device with a browser:\n https://example.com/connect?code=ABC\nConfirm this code when asked: ABC-123\nWaiting for approval (expires in 5 min).";
        let action = parse_challenge(text, 1000).unwrap();
        assert_eq!(action.url, "https://example.com/connect?code=ABC");
        assert_eq!((action.code.as_str(), action.expires_at), ("ABC-123", 1270));
        assert!(parse_challenge("https://example.com unrelated output", 1000).is_none());
        assert!(parse_challenge(&text.replace("expires in 5 min", "expires soon"), 1000).is_none());
        let long =
            parse_challenge(&text.replace("5 min", "99999999999999999999999 min"), 1000).unwrap();
        assert_eq!(long.expires_at, 2800);
        let short = parse_challenge(&text.replace("5 min", "0 min"), 1000).unwrap();
        assert_eq!(short.expires_at, 1030);
    }
}
