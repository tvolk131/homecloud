//! Explicitly scoped experimental runner; never used by the production controller.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{io::AsyncWriteExt, process::Command};
use uuid::Uuid;

mod cleanup;
mod exercise;
mod fedimint;
mod hardening;
mod setup;
mod teardown;

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Parser)]
#[command(about = "Disposable shared-Nix-store proof through Homecloud and pinned management SSH")]
struct Args {
    #[arg(long)]
    state: PathBuf,
    #[arg(long)]
    ssh_config: PathBuf,
    #[arg(long, default_value = "target/debug/homecloud")]
    cli: PathBuf,
    #[arg(long)]
    profile: Option<PathBuf>,
    #[arg(long)]
    ssh_key: Option<PathBuf>,
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..=128))]
    client_memory_gib: u64,
    #[arg(long, default_value_t = 12, value_parser = clap::value_parser!(u64).range(6..=512))]
    client_disk_gib: u64,
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..=128))]
    builder_memory_gib: u64,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(12..=512))]
    builder_disk_gib: u64,
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u64).range(8..=512))]
    store_gib: u64,
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    Prepare,
    Install {
        phase: String,
    },
    Exercise {
        phase: String,
    },
    /// Stage a clean local Fedimint HEAD, without Git credentials or untracked files.
    Fedimint {
        phase: String,
        #[arg(long)]
        source: Option<PathBuf>,
    },
    Status,
    /// Remove only this run’s shared mounts, host Nix installation, and transport.
    Uninstall,
    /// Execute an explicit host experiment after validating all fixture identities.
    Host {
        script: PathBuf,
    },
    /// Execute an experiment only in the manifest-owned guest.
    Guest {
        role: String,
        script: PathBuf,
    },
    /// Delete only manifest-owned VMs and restore the grant if it has not drifted.
    Cleanup,
}
#[derive(Serialize, Deserialize, Clone)]
struct Fixture {
    role: String,
    name: String,
    request: Uuid,
    id: Option<Uuid>,
    memory_gib: u64,
    disk_gib: u64,
    #[serde(default)]
    deletion_requested: bool,
}
#[derive(Serialize, Deserialize)]
struct State {
    version: u32,
    run: Uuid,
    owner: Uuid,
    original_grant: Value,
    applied_grant: Value,
    gpu_allowed: bool,
    protected: Value,
    fixtures: Vec<Fixture>,
    #[serde(default = "default_store_gib")]
    store_gib: u64,
    #[serde(default)]
    cleaned: bool,
    #[serde(default)]
    restoring_grant: bool,
}
impl State {
    fn check_policy(&self, snapshot: &Value, allow_original: bool) -> Result<()> {
        let owner = &snapshot["me"];
        ensure!(
            owner["id"] == self.owner.to_string() && owner["role"] == "admin",
            "Manifest administrator identity changed"
        );
        ensure!(
            owner["gpu_allowed"].as_bool() == Some(self.gpu_allowed)
                && (owner["grant"] == self.applied_grant
                    || (allow_original && owner["grant"] == self.original_grant)),
            "Grant or GPU policy drifted; preserve it for explicit review"
        );
        Ok(())
    }
}
fn default_store_gib() -> u64 {
    16
}
struct Runner {
    args: Args,
    profile: PathBuf,
    state: Option<State>,
    _lock: fs::File,
}
fn quote(s: impl AsRef<str>) -> String {
    format!("'{}'", s.as_ref().replace('\'', "'\\''"))
}
fn script_wrapper() -> &'static str {
    // Receive the entire script before invoking tools such as `incus exec` that
    // forward stdin. Otherwise they can steal the interpreter's unread source.
    "set -eu\ntask_script=$(mktemp /tmp/homecloud-nix-script.XXXXXX)\ntrap 'rm -f \"$task_script\"' EXIT\ncat > \"$task_script\"\nsh -e \"$task_script\" < /dev/null"
}
fn logged_script(body: &str, log: &str) -> String {
    let delimiter = format!("HC_LAB_{}", Uuid::new_v4().simple());
    format!(
        "phase_script=$(mktemp /tmp/homecloud-nix-phase.XXXXXX)\ntrap 'rm -f \"$phase_script\"' EXIT\ncat > \"$phase_script\" <<'{delimiter}'\nset -eu\n{body}\n{delimiter}\nif sh -e \"$phase_script\" > {} 2>&1; then tail -100 {}; else tail -100 {}; exit 1; fi\n",
        quote(log),
        quote(log),
        quote(log)
    )
}
async fn protected_inventory<Query, Reply>(mut query: Query) -> Result<Value>
where
    Query: FnMut(String) -> Reply,
    Reply: std::future::Future<Output = Result<String>>,
{
    let inventory: Value = serde_json::from_str(&query("incus list --format=json".into()).await?)?;
    let mut protected = Vec::new();
    for instance in inventory.as_array().context("Incus instance inventory")? {
        let name = instance["name"].as_str().context("Incus instance name")?;
        let script = format!(
            "incus query {}",
            quote(format!("/1.0/instances/{name}/state"))
        );
        let state: Value = serde_json::from_str(&query(script).await?)?;
        let pid = state["pid"].as_i64().context("Incus instance process ID")?;
        protected.push(json!({"name":name,"pid":pid}));
    }
    Ok(json!(protected))
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    std::io::Write::write_all(&mut f, bytes)?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    fs::File::open(path.parent().context("Parent directory")?)?.sync_all()?;
    Ok(())
}
async fn execute(mut command: Command, input: Option<&[u8]>, seconds: u64) -> Result<String> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("Start experiment command")?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .context("Command stdin")?
            .write_all(input)
            .await?;
    }
    let output = tokio::time::timeout(Duration::from_secs(seconds), child.wait_with_output())
        .await
        .context("Command deadline reached; inspect retained fixtures before retrying")??;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    ensure!(
        output.status.success(),
        "Command failed ({}): {}\n{}",
        output.status,
        stdout
            .chars()
            .rev()
            .take(5000)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>(),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(stdout)
}
impl Runner {
    async fn new(args: Args) -> Result<Self> {
        let parent = args.state.parent().context("State parent")?;
        fs::create_dir_all(parent)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(parent.join(".homecloud-lab.lock"))?;
        lock.try_lock()
            .context("Another lab runner holds the lock")?;
        let state: Option<State> = if args.state.exists() {
            Some(serde_json::from_slice(&fs::read(&args.state)?)?)
        } else {
            None
        };
        if let Some(s) = &state {
            ensure!(s.version == 1, "Unsupported manifest version");
        }
        let profile = args.state.with_extension("profile.json");
        let mut runner = Self {
            args,
            profile,
            state,
            _lock: lock,
        };
        let home = std::env::var("HOME")?;
        let original = runner
            .args
            .profile
            .clone()
            .unwrap_or_else(|| PathBuf::from(home).join(".config/homecloud/client.json"));
        let mut p: Value = serde_json::from_slice(&fs::read(original)?)?;
        let discovery: Value = serde_json::from_str(
            &runner
                .remote("cat /var/lib/homecloud/control/server.json", 30)
                .await?,
        )?;
        ensure!(
            p["server"]["id"] == discovery["id"],
            "Public discovery changed the pinned host identity"
        );
        p["server"] = discovery;
        private_write(&runner.profile, &serde_json::to_vec_pretty(&p)?)?;
        if let Some(s) = &mut runner.state {
            ensure!(
                !s.cleaned || matches!(runner.args.action, Action::Status),
                "This proof was cleaned"
            );
        }
        Ok(runner)
    }
    fn save(&self) -> Result<()> {
        private_write(&self.args.state, &serde_json::to_vec_pretty(&self.state)?)
    }
    async fn remote(&self, script: &str, seconds: u64) -> Result<String> {
        let mut c = Command::new("ssh");
        c.arg("-F").arg(&self.args.ssh_config).args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ServerAliveInterval=10",
            "-o",
            "ServerAliveCountMax=3",
            "hclab-host",
        ]);
        c.arg(format!("sudo -n sh -c {}", quote(script_wrapper())));
        execute(c, Some(script.as_bytes()), seconds).await
    }
    async fn cli(&self, args: &[String]) -> Result<Value> {
        let mut c = Command::new(&self.args.cli);
        c.arg("--profile").arg(&self.profile).args(args);
        let output = execute(c, None, 55).await?;
        let reply: Value = serde_json::from_str(&output).context("Homecloud response")?;
        ensure!(
            reply["kind"] != "error",
            "Homecloud rejected request: {}",
            reply["data"]
        );
        Ok(reply["data"].clone())
    }
    async fn snapshot(&self) -> Result<Value> {
        self.cli(&["status".into()]).await
    }
    async fn grant(&self, grant: &Value, allow_original: bool) -> Result<()> {
        let s = self.state.as_ref().context("No manifest")?;
        let snapshot = self.snapshot().await?;
        s.check_policy(&snapshot, allow_original)?;
        if snapshot["me"]["grant"] == *grant {
            return Ok(());
        }
        let mut args = vec![
            "grant".into(),
            s.owner.to_string(),
            "--memory".into(),
            (grant["memory_bytes"].as_u64().context("RAM grant")? / GIB).to_string(),
            "--cores".into(),
            grant["cpu_cores"].to_string(),
            "--disk".into(),
            (grant["disk_bytes"].as_u64().context("Disk grant")? / GIB).to_string(),
        ];
        if s.gpu_allowed {
            args.push("--gpu".into());
        }
        self.cli(&args).await?;
        Ok(())
    }
    async fn prepare(&mut self) -> Result<()> {
        let snapshot = self.snapshot().await?;
        if self.state.is_none() {
            ensure!(snapshot["me"]["role"] == "admin", "Administrator required");
            let run = Uuid::new_v4();
            let prefix = format!("hc-proof-nix-{}", &run.simple().to_string()[..10]);
            let runner = &*self;
            let protected =
                protected_inventory(|script| async move { runner.remote(&script, 30).await })
                    .await?;
            let grant = snapshot["me"]["grant"].clone();
            let used = &snapshot["me"]["committed"];
            let mut applied = grant.clone();
            applied["memory_bytes"] = json!(grant["memory_bytes"].as_u64().unwrap().max(
                used["memory_bytes"].as_u64().unwrap()
                    + (2 * self.args.client_memory_gib + self.args.builder_memory_gib) * GIB
            ));
            applied["cpu_cores"] = json!(
                grant["cpu_cores"]
                    .as_u64()
                    .unwrap()
                    .max(used["cpu_cores"].as_u64().unwrap() + 3)
            );
            applied["disk_bytes"] = json!(grant["disk_bytes"].as_u64().unwrap().max(
                used["disk_bytes"].as_u64().unwrap()
                    + (2 * self.args.client_disk_gib + self.args.builder_disk_gib + 6) * GIB
            ));
            ensure!(
                applied["disk_bytes"].as_u64().unwrap()
                    <= snapshot["capacity"]["disk_bytes"].as_u64().unwrap(),
                "Insufficient reviewed disk capacity"
            );
            self.state = Some(State {
                version: 1,
                run,
                owner: serde_json::from_value(snapshot["me"]["id"].clone())?,
                original_grant: grant,
                applied_grant: applied,
                gpu_allowed: snapshot["me"]["gpu_allowed"].as_bool().unwrap_or(false),
                protected,
                fixtures: ["client-a", "client-b", "builder"]
                    .iter()
                    .map(|role| Fixture {
                        role: (*role).into(),
                        name: format!("{prefix}-{role}"),
                        request: Uuid::new_v4(),
                        id: None,
                        memory_gib: if *role == "builder" {
                            self.args.builder_memory_gib
                        } else {
                            self.args.client_memory_gib
                        },
                        disk_gib: if *role == "builder" {
                            self.args.builder_disk_gib
                        } else {
                            self.args.client_disk_gib
                        },
                        deletion_requested: false,
                    })
                    .collect(),
                store_gib: self.args.store_gib,
                cleaned: false,
                restoring_grant: false,
            });
            self.save()?;
        }
        let state = self.state.as_ref().unwrap();
        ensure!(
            !state.restoring_grant
                && !state
                    .fixtures
                    .iter()
                    .any(|fixture| fixture.deletion_requested),
            "Cleanup has started; resume cleanup instead of preparation"
        );
        self.grant(&state.applied_grant, true).await?;
        let home = std::env::var("HOME")?;
        let key = self
            .args
            .ssh_key
            .clone()
            .unwrap_or_else(|| PathBuf::from(home).join(".ssh/id_ed25519.pub"));
        for i in 0..3 {
            let f = self.state.as_ref().unwrap().fixtures[i].clone();
            if f.id.is_some() {
                continue;
            }
            let current = self.snapshot().await?;
            if let Some(v) = current["vms"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["spec"]["name"] == f.name)
            {
                ensure!(
                    v["owner"] == self.state.as_ref().unwrap().owner.to_string(),
                    "Fixture owner mismatch"
                );
                self.state.as_mut().unwrap().fixtures[i].id =
                    Some(serde_json::from_value(v["id"].clone())?);
            } else {
                let op = self
                    .cli(&[
                        "create".into(),
                        f.name,
                        "--memory".into(),
                        f.memory_gib.to_string(),
                        "--cores".into(),
                        "1".into(),
                        "--disk".into(),
                        f.disk_gib.to_string(),
                        "--ssh-key".into(),
                        key.to_string_lossy().into_owned(),
                        "--request-id".into(),
                        f.request.to_string(),
                    ])
                    .await?;
                self.state.as_mut().unwrap().fixtures[i].id =
                    Some(serde_json::from_value(op["vm_id"].clone())?);
            }
            self.save()?;
            eprintln!("Recorded {}", f.role);
        }
        self.wait().await?;
        self.verify().await?;
        println!("{}", serde_json::to_string_pretty(&self.state)?);
        Ok(())
    }
    async fn wait(&self) -> Result<()> {
        let start = Instant::now();
        loop {
            let snapshot = self.snapshot().await?;
            let s = self.state.as_ref().unwrap();
            let mut ready = true;
            for f in &s.fixtures {
                let Some(id) = f.id else { continue };
                let vm = snapshot["vms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["id"] == id.to_string());
                let Some(v) = vm else {
                    ready = false;
                    continue;
                };
                ensure!(
                    v["owner"] == s.owner.to_string()
                        && v["spec"]["name"] == f.name
                        && v["spec"]["applications"]
                            .as_array()
                            .is_none_or(Vec::is_empty),
                    "Fixture identity or application changed"
                );
                ensure!(
                    v["error"].is_null()
                        || v["error"]
                            .as_str()
                            .is_some_and(|e| e.starts_with("pending:")),
                    "Fixture needs inspection: {}",
                    v["error"]
                );
                ready &= v["error"].is_null();
                ready &= v["observed"] == "running";
                for op in snapshot["operations"].as_array().unwrap() {
                    if op["vm_id"] == id.to_string() {
                        ensure!(
                            op["phase"] != "needs_review" && op["phase"] != "failed",
                            "Fixture operation failed: {}",
                            op
                        );
                        ready &= op["phase"] == "succeeded";
                    }
                }
            }
            if ready {
                return Ok(());
            }
            ensure!(
                start.elapsed() < Duration::from_secs(600),
                "Provisioning deadline; fixtures retained"
            );
            eprintln!("Waiting for fixture lifecycle...");
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
    fn fence(&self) -> Result<String> {
        let s = self.state.as_ref().context("Prepare fixtures first")?;
        let mut script = String::from("set -eu\n");
        for f in &s.fixtures {
            script.push_str(&fixture_fence(s, f)?);
        }
        script.push_str(&self.protected_fence()?);
        Ok(script)
    }
    fn protected_fence(&self) -> Result<String> {
        let s = self.state.as_ref().context("Prepare fixtures first")?;
        let mut script = String::new();
        for p in s.protected.as_array().context("Protected VMs")? {
            script.push_str(&format!(
                "test \"$(incus query {} | jq -r '.pid // 0')\" = {}\n",
                quote(format!(
                    "/1.0/instances/{}/state",
                    p["name"].as_str().unwrap()
                )),
                quote(p["pid"].to_string())
            ));
        }
        Ok(script)
    }
    async fn verify(&self) -> Result<()> {
        self.remote(&self.fence()?, 40).await?;
        Ok(())
    }
    async fn experiment(&self, role: Option<&str>, script_path: &Path) -> Result<()> {
        self.verify().await?;
        let script = fs::read_to_string(script_path)?;
        let mut command = self.fence()?;
        if let Some(role) = role {
            let f = self
                .state
                .as_ref()
                .unwrap()
                .fixtures
                .iter()
                .find(|f| f.role == role)
                .context("Unknown fixture role")?;
            let name = format!("hc-{}", f.id.unwrap().simple());
            command.push_str(&format!(
                "incus exec {} -- sh -se -c {}\n",
                quote(name),
                quote(&script)
            ));
        } else {
            command.push_str(&script);
        }
        let output = self.remote(&command, 1200).await?;
        print!("{output}");
        self.verify().await?;
        Ok(())
    }
}
fn fixture_fence(state: &State, fixture: &Fixture) -> Result<String> {
    let id = fixture.id.context("Fixture ID missing")?;
    let name = format!("hc-{}", id.simple());
    Ok(format!(
        "test \"$(incus config get {} user.homecloud.id)\" = {}\ntest \"$(incus config get {} user.homecloud.owner)\" = {}\n",
        quote(&name),
        quote(id.to_string()),
        quote(&name),
        quote(state.owner.to_string())
    ))
}
#[tokio::main]
async fn main() -> Result<()> {
    let mut runner = Runner::new(Args::parse()).await?;
    match &runner.args.action {
        Action::Prepare => runner.prepare().await,
        Action::Install { phase } => runner.install(phase).await,
        Action::Exercise { phase } => runner.exercise(phase).await,
        Action::Uninstall => runner.uninstall().await,
        Action::Fedimint { phase, source } => runner.fedimint(phase, source.as_deref()).await,
        Action::Status => {
            println!(
                "{}",
                serde_json::to_string_pretty(&runner.snapshot().await?)?
            );
            Ok(())
        }
        Action::Host { script } => runner.experiment(None, script).await,
        Action::Guest { role, script } => runner.experiment(Some(role), script).await,
        Action::Cleanup => runner.cleanup().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn protected_inventory_rejects_failed_or_incomplete_queries() {
        assert!(
            protected_inventory(|_| std::future::ready(Err(anyhow::anyhow!("list failed"))))
                .await
                .is_err()
        );
        let mut queries = 0;
        let result = protected_inventory(|_| {
            queries += 1;
            std::future::ready(match queries {
                1 => Ok(r#"[{"name":"first"},{"name":"second"}]"#.into()),
                2 => Ok(r#"{"pid":123}"#.into()),
                _ => Err(anyhow::anyhow!("state query failed")),
            })
        })
        .await;
        assert!(result.is_err());
        assert_eq!(queries, 3);
        for invalid in ["not JSON", "null", "{}", r#"{"pid":"unknown"}"#] {
            let result = protected_inventory(|script| {
                std::future::ready(Ok(if script == "incus list --format=json" {
                    r#"[{"name":"fixture"}]"#.into()
                } else {
                    invalid.into()
                }))
            })
            .await;
            assert!(
                result.is_err(),
                "accepted incomplete instance state: {invalid}"
            );
        }
    }

    #[tokio::test]
    async fn protected_inventory_records_every_confirmed_process() {
        let protected = protected_inventory(|script| {
            std::future::ready(Ok(match script.as_str() {
                "incus list --format=json" => r#"[{"name":"running"},{"name":"stopped"}]"#,
                "incus query '/1.0/instances/running/state'" => r#"{"pid":123}"#,
                "incus query '/1.0/instances/stopped/state'" => r#"{"pid":0}"#,
                _ => panic!("unexpected query: {script}"),
            }
            .into()))
        })
        .await
        .unwrap();
        assert_eq!(
            protected,
            json!([{"name":"running","pid":123},{"name":"stopped","pid":0}])
        );
        assert_eq!(
            protected_inventory(|_| std::future::ready(Ok("[]".into())))
                .await
                .unwrap(),
            json!([])
        );
    }

    #[test]
    fn shell_quote_preserves_literal_metacharacters() {
        let input = "a' $(touch /tmp/no) `id`\n b";
        let output = std::process::Command::new("sh")
            .args(["-c", &format!("printf %s {}", quote(input))])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, input.as_bytes());
    }
    #[tokio::test]
    async fn child_cannot_consume_the_interpreters_unread_script() {
        let script = format!(
            "cat > /dev/null\n# {}\nprintf '%s' completed\n",
            "padding".repeat(16000)
        );
        let mut command = Command::new("sh");
        command.args(["-c", script_wrapper()]);
        assert_eq!(
            execute(command, Some(script.as_bytes()), 30).await.unwrap(),
            "completed"
        );
    }
    #[tokio::test]
    async fn logging_preserves_workload_failure() {
        let dir = std::env::temp_dir().join(format!("hcnix-test-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let log = dir.join("phase.log");
        let body = logged_script("false\nprintf should-not-run", log.to_str().unwrap());
        let mut command = Command::new("sh");
        command.args(["-c", script_wrapper()]);
        assert!(execute(command, Some(body.as_bytes()), 30).await.is_err());
        assert!(!fs::read_to_string(&log).unwrap().contains("should-not-run"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn replacement_keeps_state_private_and_complete() {
        let dir = std::env::temp_dir().join(format!("hcnix-test-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("state.json");
        private_write(&path, br#"{"version":1}"#).unwrap();
        private_write(&path, br#"{"version":2}"#).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"version":2}"#);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
