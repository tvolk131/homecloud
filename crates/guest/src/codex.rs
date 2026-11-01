//! Codex-owned OAuth over its versioned stdio app-server protocol.
//!
//! Only normalized account state, display metadata and a public device
//! challenge leave this adapter. No model requests, external-token auth or
//! credential-file reads are needed.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::process::{ChildStdin, ChildStdout, Stdio};
use std::time::{Duration, Instant};

use homecloud_core::{ApplicationSetupAction, BrowserAction};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::ctx::Ctx;
use crate::error::{Result, fail};
use crate::metadata::{AccountInfo, AccountState, codex_account};
use crate::process::{self, Guard};
use crate::state::Job;

const DEVICE_URL: &str = "https://auth.openai.com/codex/device";
pub const MAX_FRAME: usize = 1024 * 1024;
const FAILURE: &str = "Codex sign-in did not finish. Check internet access and device-code login in \
                       ChatGPT Settings > Security (or ask your workspace administrator), then retry.";
const STOPPED: &str = "Codex sign-in process stopped. Try again.";
const UNEXPECTED: &str = "Unexpected Codex login response.";
const MALFORMED: &str = "Unexpected Codex login response. Check the installed CLI version.";

pub struct Session {
    child: Guard,
    stdin: ChildStdin,
    stdout: ChildStdout,
    buffer: Vec<u8>,
    max_frame: usize,
    notifications: VecDeque<Value>,
    sequence: u64,
    login_id: Option<Uuid>,
}

impl Session {
    pub fn start(ctx: &Ctx) -> Result<Self> {
        let mut session = Self::spawn(ctx, MAX_FRAME)?;
        let params = json!({"clientInfo": {"name": "homecloud", "version": "0.1.0"},
                            "capabilities": {"experimentalApi": false}});
        session.request("initialize", params, Duration::from_secs(15))?;
        session.send(&json!({"method": "initialized"}))?;
        Ok(session)
    }

    fn spawn(ctx: &Ctx, max_frame: usize) -> Result<Self> {
        let Some(binary) = ctx.which("codex") else {
            return fail("Install the Codex CLI before signing in.");
        };
        let mut command = ctx.command(binary);
        command
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        process::new_session(&mut command);
        let mut child = Guard::new(command.spawn()?, Duration::from_secs(3));
        let stdin = child.0.stdin.take().expect("stdin is piped");
        let stdout = child.0.stdout.take().expect("stdout is piped");
        Ok(Self {
            child,
            stdin,
            stdout,
            buffer: Vec::new(),
            max_frame,
            notifications: VecDeque::new(),
            sequence: 0,
            login_id: None,
        })
    }

    fn send(&mut self, value: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        match self
            .stdin
            .write_all(&line)
            .and_then(|()| self.stdin.flush())
        {
            Ok(()) => Ok(()),
            Err(_) => fail(STOPPED),
        }
    }

    fn receive(&mut self, deadline: Instant) -> Result<Map<String, Value>> {
        let mut chunk = [0u8; 8192];
        while !self.buffer.contains(&b'\n') {
            if !process::readable(self.stdout.as_fd(), deadline)? {
                return fail("Codex sign-in timed out. Request a new sign-in link.");
            }
            let count = match self.stdout.read(&mut chunk) {
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => 0,
            };
            if count == 0 {
                return fail(STOPPED);
            }
            self.buffer.extend_from_slice(&chunk[..count]);
            if self.buffer.len() > self.max_frame {
                return fail(MALFORMED);
            }
        }
        let end = self
            .buffer
            .iter()
            .position(|b| *b == b'\n')
            .expect("a complete line");
        let line: Vec<u8> = self.buffer.drain(..=end).collect();
        match serde_json::from_slice::<Value>(&line[..end]) {
            Ok(Value::Object(value)) => Ok(value),
            _ => fail(MALFORMED),
        }
    }

    pub fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Map<String, Value>> {
        self.sequence += 1;
        let id = self.sequence;
        self.send(&json!({"id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        loop {
            let value = self.receive(deadline)?;
            if value.get("id").and_then(Value::as_u64) == Some(id) && !value.contains_key("method")
            {
                if value.contains_key("error") {
                    // Upstream error text can contain identities or URLs. Never persist it.
                    return fail(FAILURE);
                }
                return match value.get("result") {
                    Some(Value::Object(result)) => Ok(result.clone()),
                    _ => fail(UNEXPECTED),
                };
            }
            self.accept(value)?;
        }
    }

    fn accept(&mut self, value: Map<String, Value>) -> Result<()> {
        if let (Some(id), true) = (value.get("id"), value.contains_key("method")) {
            let reply =
                json!({"id": id, "error": {"code": -32601, "message": "Unsupported method"}});
            return self.send(&reply);
        }
        if value.get("method").and_then(Value::as_str) == Some("account/login/completed") {
            if self.notifications.len() >= 32 {
                return fail(UNEXPECTED);
            }
            self.notifications.push_back(Value::Object(value));
        }
        Ok(())
    }

    fn read_account(&mut self) -> Result<AccountInfo> {
        let result = self.request(
            "account/read",
            json!({"refreshToken": false}),
            Duration::from_secs(15),
        )?;
        Ok(codex_account(&Value::Object(result)))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(login_id) = self.login_id.take()
            && matches!(self.child.exited(), Ok(None))
        {
            let params = json!({"loginId": login_id.to_string()});
            let _ = self.request("account/login/cancel", params, Duration::from_secs(2));
        }
        self.child.stop();
    }
}

pub fn account_info(ctx: &Ctx) -> AccountInfo {
    if ctx.which("codex").is_none() {
        return AccountState::Unavailable.into();
    }
    Session::start(ctx)
        .and_then(|mut session| session.read_account())
        .unwrap_or_else(|_| AccountState::Unknown.into())
}

fn user_code(code: &str) -> bool {
    (4..=32).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn challenge(
    result: &Map<String, Value>,
    now: u64,
    timeout: Duration,
) -> Result<(Uuid, BrowserAction)> {
    let text = |key| result.get(key).and_then(Value::as_str);
    let code = text("userCode").filter(|code| user_code(code));
    // Never turn an arbitrary guest-supplied URL into a credential collection page.
    let (Some("chatgptDeviceCode"), Some(DEVICE_URL), Some(code)) =
        (text("type"), text("verificationUrl"), code)
    else {
        return fail("Unexpected Codex authorization page. Check the installed CLI version.");
    };
    let Some(login_id) = text("loginId").and_then(|id| Uuid::parse_str(id).ok()) else {
        return fail(UNEXPECTED);
    };
    Ok((
        login_id,
        BrowserAction {
            url: DEVICE_URL.into(),
            code: code.into(),
            expires_at: now + timeout.as_secs(),
            setup_action: ApplicationSetupAction::SignInCodex,
            input_required: false,
        },
    ))
}

pub fn authorize(ctx: &Ctx, job: &mut Job) -> Result<()> {
    let mut session = Session::start(ctx)?;
    match session.read_account()?.state {
        AccountState::SignedIn => return Ok(()), // Never overwrite an existing account.
        AccountState::SignedOut => {}
        _ => {
            return fail(
                "Could not inspect the existing Codex account. Refresh its status before signing in.",
            );
        }
    }
    let result = session.request(
        "account/login/start",
        json!({"type": "chatgptDeviceCode"}),
        Duration::from_secs(45),
    )?;
    let (login_id, action) = challenge(&result, crate::now(), ctx.login_timeout)?;
    session.login_id = Some(login_id);
    job.state.message = Some("Approve Codex sign-in in your browser.".into());
    job.awaiting(action)?;
    let deadline = Instant::now() + ctx.login_timeout;
    loop {
        let Some(notification) = session.notifications.pop_front() else {
            let value = session.receive(deadline)?;
            session.accept(value)?;
            continue;
        };
        let params = notification.get("params").cloned().unwrap_or_default();
        let matches = params
            .get("loginId")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            == Some(login_id);
        if !matches {
            continue;
        }
        if params.get("success") != Some(&Value::Bool(true)) {
            return fail(FAILURE);
        }
        job.verifying()?;
        // Check the credential store through Codex; a notification alone isn't proof.
        if session.read_account()?.state != AccountState::SignedIn {
            return fail("Could not confirm saved Codex sign-in. Try again.");
        }
        session.login_id = None;
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;
    use homecloud_core::ApplicationPhase;

    const FAKE: &str = r#"
import json,sys
mode = MODE
signed = mode == 'already'
for line in sys.stdin:
 value=json.loads(line)
 if 'method' not in value: continue
 method=value['method']; params=value.get('params',{}); id=value.get('id')
 if method=='initialized': continue
 if method=='initialize': result={}
 elif method=='account/read': result={'account': {'type':'chatgpt','email':'PRIVATE-IDENTITY'} if signed else None}
 elif method=='account/login/start':
  if mode=='error':
   print(json.dumps({'id':id,'error':{'code':-1,'message':'PRIVATE-TOKEN https://secret.invalid'}}),flush=True); continue
  result={'type':'chatgptDeviceCode','loginId':'00000000-0000-4000-8000-000000000003',
          'verificationUrl':'https://auth.openai.com/codex/device','userCode':'DEMO-1234'}
  if mode=='wrong_url': result['verificationUrl']='https://evil.invalid/capture'
 elif method=='account/login/cancel': result={}
 else: sys.exit(4)
 print(json.dumps({'id':id,'result':result}),flush=True)
 if method=='account/login/start':
  print(json.dumps({'id':99,'method':'item/tool/requestUserInput','params':{}}),flush=True)
  print(json.dumps({'method':'account/login/completed','params':{'loginId':'wrong-job','success':True}}),flush=True)
  signed=mode!='unsaved'
  print(json.dumps({'method':'account/login/completed','params':{'loginId':result['loginId'],'success':mode!='denied'}}),flush=True)
"#;

    fn fixture(body: &str) -> Fixture {
        let fx = Fixture::new();
        fx.python_tool("codex", body);
        fx
    }

    fn flow(mode: &str) -> (Job, Option<String>) {
        let fx = fixture(&FAKE.replace("MODE", &serde_json::to_string(mode).unwrap()));
        let mut job = crate::state::test_job(fx.root.path(), ApplicationSetupAction::SignInCodex);
        let error = authorize(&fx.ctx, &mut job)
            .err()
            .map(|e| e.user_message().to_owned());
        (job, error)
    }

    #[test]
    fn account_metadata_uses_the_supported_read_only_method() {
        let fx = fixture(&FAKE.replace("MODE", "'already'"));
        let info = account_info(&fx.ctx);
        assert_eq!(info.state, AccountState::SignedIn);
        assert_eq!(info.email.as_deref(), Some("PRIVATE-IDENTITY"));
    }

    #[test]
    fn structured_login_correlates_completion_and_confirms_saved_account() {
        let (job, error) = flow("success");
        assert_eq!(error, None);
        assert_eq!(job.history.len(), 2);
        assert_eq!(job.history[0].phase, ApplicationPhase::WaitingForUser);
        let action = job.history[0].action.as_ref().unwrap();
        assert_eq!(
            (action.code.as_str(), action.setup_action),
            ("DEMO-1234", ApplicationSetupAction::SignInCodex)
        );
        assert!(
            !serde_json::to_string(&job.history)
                .unwrap()
                .contains("PRIVATE-IDENTITY")
        );
    }

    #[test]
    fn existing_account_is_not_replaced_or_prompted() {
        let (job, error) = flow("already");
        assert_eq!((job.history.len(), error), (0, None));
    }

    #[test]
    fn failure_discards_upstream_secrets_and_has_actionable_copy() {
        let (job, error) = flow("error");
        let error = error.unwrap();
        assert!(job.history.is_empty());
        assert!(error.contains("Settings > Security"));
        assert!(!error.contains("PRIVATE") && !error.contains("secret.invalid"));
    }

    #[test]
    fn arbitrary_browser_url_is_rejected() {
        let (job, error) = flow("wrong_url");
        assert!(job.history.is_empty());
        assert!(
            error
                .unwrap()
                .contains("Unexpected Codex authorization page")
        );
    }

    #[test]
    fn denial_or_notification_without_saved_credentials_is_not_success() {
        for (mode, writes) in [("denied", 1), ("unsaved", 2)] {
            let (job, error) = flow(mode);
            assert_eq!(job.history.len(), writes, "{mode}");
            assert!(error.is_some());
        }
    }

    #[test]
    fn timeout_and_oversized_or_malformed_frames_are_bounded() {
        for body in [
            "import time; time.sleep(5)",
            "print('x'*4096,flush=True)",
            "print('not-json',flush=True)",
        ] {
            let fx = fixture(body);
            let started = Instant::now();
            let mut session = Session::spawn(&fx.ctx, 1024).unwrap();
            assert!(
                session
                    .request("initialize", json!({}), Duration::from_millis(150))
                    .is_err()
            );
            drop(session);
            assert!(started.elapsed() < Duration::from_secs(4), "{body}");
        }
    }

    #[test]
    fn challenge_has_a_local_waiting_deadline_not_an_invented_upstream_expiry() {
        let result = json!({"type": "chatgptDeviceCode", "loginId": "00000000-0000-4000-8000-000000000003",
                            "verificationUrl": DEVICE_URL, "userCode": "DEMO-1234"});
        let (_, action) =
            challenge(result.as_object().unwrap(), 1000, Duration::from_secs(600)).unwrap();
        assert_eq!(action.expires_at, 1600);
    }
}
