use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use homecloud_core::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    task::JoinSet,
};

mod parse;
#[cfg(test)]
mod tests;

const SMART_INTERVAL: u64 = 15 * 60;
const SMART_BATCH: usize = 32;
const SMART_WORKERS: usize = 4;
const MAX_OUTPUT: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
enum Probe {
    Disks,
    Pools,
    Smart { path: String, nvme: bool },
}

impl Probe {
    fn command(&self) -> (&'static str, Vec<String>, Duration) {
        let (program, args, timeout) = match self {
            Self::Disks => (
                "lsblk",
                vec![
                    "--json",
                    "--bytes",
                    "--paths",
                    "--output",
                    "NAME,KNAME,PATH,SIZE,TYPE,MODEL,SERIAL,WWN,TRAN,ROTA,FSTYPE,MOUNTPOINTS",
                ],
                10,
            ),
            Self::Pools => ("zpool", vec!["status", "-j", "--json-int", "-P", "-p"], 15),
            Self::Smart { path, nvme } => {
                let mut args = vec!["-j", "-x"];
                if !nvme {
                    args.extend(["-n", "standby,99,98"]);
                }
                args.push(path);
                ("smartctl", args, 8)
            }
        };
        (
            program,
            args.into_iter().map(str::to_owned).collect(),
            Duration::from_secs(timeout),
        )
    }
}

struct ProbeOutput {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ProbeOutput {
    fn json(&self, program: &str) -> Result<serde_json::Value> {
        ensure!(
            self.code == 0,
            "{program}: {}",
            String::from_utf8_lossy(&self.stderr)
                .chars()
                .take(512)
                .collect::<String>()
        );
        serde_json::from_slice(&self.stdout).with_context(|| format!("Invalid {program} JSON"))
    }
}

#[async_trait]
trait Runner: Clone + Send + Sync + 'static {
    async fn run(&self, probe: Probe) -> Result<ProbeOutput>;
}

#[derive(Clone)]
struct SystemRunner;

async fn read_output(stream: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream.take(MAX_OUTPUT + 1).read_to_end(&mut bytes).await?;
    ensure!(
        bytes.len() as u64 <= MAX_OUTPUT,
        "Storage command output exceeds limit"
    );
    Ok(bytes)
}

async fn run_command(program: &str, args: &[String], timeout: Duration) -> Result<ProbeOutput> {
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("Starting {program}"))?;
    let stdout = child
        .stdout
        .take()
        .context("Missing storage command stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("Missing storage command stderr")?;
    tokio::time::timeout(timeout, async {
        let (status, stdout, stderr) = tokio::try_join!(
            async { child.wait().await.map_err(anyhow::Error::from) },
            read_output(stdout),
            read_output(stderr)
        )?;
        Ok(ProbeOutput {
            code: status.code().unwrap_or(-1),
            stdout,
            stderr,
        })
    })
    .await
    .with_context(|| format!("{program} timed out"))?
}

fn collection_error(error: &anyhow::Error) -> String {
    format!("{error:#}").chars().take(512).collect()
}

#[async_trait]
impl Runner for SystemRunner {
    async fn run(&self, probe: Probe) -> Result<ProbeOutput> {
        let (program, args, timeout) = probe.command();
        run_command(program, &args, timeout).await
    }
}

async fn probe_smart(
    runner: impl Runner,
    disk: &PhysicalDisk,
    previous: SmartObservation,
    now: u64,
) -> SmartObservation {
    let probe = Probe::Smart {
        path: disk.path.clone(),
        nvme: disk.transport.as_deref() == Some("nvme")
            || Path::new(&disk.path)
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("nvme")),
    };
    let result = async {
        let output = runner.run(probe).await?;
        if let Some(skipped) = parse::skipped_smart(output.code, previous.clone(), now) {
            return Ok(skipped);
        }
        let payload: serde_json::Value =
            serde_json::from_slice(&output.stdout).context("Invalid smartctl JSON")?;
        if let Some(reported) = parse::text(&payload["serial_number"])
            && disk
                .serial
                .as_ref()
                .is_some_and(|serial| serial != &reported)
        {
            return Ok(parse::smart_failure(
                SmartObservation::default(),
                now,
                SmartState::Unavailable,
                "SMART serial differs from disk inventory; reading discarded",
            ));
        }
        Ok::<_, anyhow::Error>(parse::smart(&payload, output.code, now, previous.clone()))
    }
    .await;
    result.unwrap_or_else(|error| {
        parse::smart_failure(
            previous,
            now,
            SmartState::Unavailable,
            collection_error(&error),
        )
    })
}

async fn discover(
    runner: impl Runner,
    previous: &StorageInventory,
    now: u64,
    batch: usize,
) -> Result<(Vec<PhysicalDisk>, bool)> {
    let mut disks = parse::disks(&runner.run(Probe::Disks).await?.json("lsblk")?)?;
    let mut previous_identities = BTreeMap::<String, usize>::new();
    for disk in &previous.disks {
        if let Some(identity) = parse::disk_identity(
            disk.model.as_deref(),
            disk.serial.as_deref(),
            disk.wwn.as_deref(),
        ) {
            *previous_identities.entry(identity).or_default() += 1;
        }
    }
    let previous: BTreeMap<_, _> = previous.disks.iter().map(|disk| (&disk.id, disk)).collect();
    let mut pending = Vec::new();
    for (index, disk) in disks.iter_mut().enumerate() {
        let stable_identity = !disk.id.starts_with("path:");
        if let Some(old) = previous.get(&disk.id)
            && old.model == disk.model
            && old.serial == disk.serial
            && old.wwn == disk.wwn
            && old.size_bytes == disk.size_bytes
        {
            if stable_identity && previous_identities.get(&disk.id) == Some(&1) {
                disk.smart = old.smart.clone();
            } else if !stable_identity {
                disk.smart.collection.attempted_at = old.smart.collection.attempted_at;
            }
        }
        let attempted = disk.smart.collection.attempted_at;
        if !stable_identity
            || attempted
                .is_none_or(|attempted| attempted > now || now - attempted >= SMART_INTERVAL)
        {
            pending.push((attempted.filter(|attempted| *attempted <= now), index));
        }
    }
    pending.sort_by_key(|(attempted, _)| *attempted);
    let remaining = pending.len() > batch;
    let pending: Vec<_> = pending.into_iter().take(batch).collect();
    for group in pending.chunks(SMART_WORKERS) {
        let mut workers = JoinSet::new();
        for &(_, index) in group {
            let runner = runner.clone();
            let disk = disks[index].clone();
            workers.spawn(async move {
                let smart = probe_smart(runner, &disk, disk.smart.clone(), now).await;
                (index, smart)
            });
        }
        while let Some(result) = workers.join_next().await {
            let (index, smart) = result.context("SMART worker failed")?;
            disks[index].smart = smart;
        }
    }
    Ok((disks, remaining))
}

async fn observe(
    runner: impl Runner,
    managed_pool: &str,
    mut previous: StorageInventory,
    now: u64,
    batch: usize,
) -> StorageInventory {
    previous.warnings.clear();
    previous.disk_collection.attempted_at = Some(now);
    previous.pool_collection.attempted_at = Some(now);
    match discover(runner.clone(), &previous, now, batch).await {
        Ok((disks, remaining)) => {
            previous.disks = disks;
            previous.disk_collection.observed_at = Some(now);
            previous.disk_collection.error = None;
            if remaining {
                previous.warnings.push("SMART probes are bounded per collection; remaining disks will be sampled next cycle".into());
            }
        }
        Err(error) => previous.disk_collection.error = Some(collection_error(&error)),
    }
    let pools = async {
        let output = runner.run(Probe::Pools).await?;
        let disks = if previous.disk_collection.error.is_none() {
            &previous.disks[..]
        } else {
            &[]
        };
        parse::pools(&output.json("zpool")?, disks, managed_pool)
    }
    .await;
    match pools {
        Ok(pools) => {
            previous.pools = pools;
            previous.pool_collection.observed_at = Some(now);
            previous.pool_collection.error = None;
        }
        Err(error) => previous.pool_collection.error = Some(collection_error(&error)),
    }
    previous
}

#[derive(Serialize, Deserialize)]
struct Cache {
    version: u32,
    inventory: StorageInventory,
}

fn load_cache(state: &Path) -> StorageInventory {
    let record = (|| {
        let mut bytes = Vec::new();
        File::open(state.join("storage.json"))?
            .take(MAX_OUTPUT + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_OUTPUT,
            "Storage cache exceeds limit"
        );
        let cache: Cache = serde_json::from_slice(&bytes)?;
        ensure!(cache.version == 1, "Unsupported storage cache version");
        Ok::<_, anyhow::Error>(cache.inventory)
    })();
    record.unwrap_or_default()
}

fn save_cache(state: &Path, inventory: &StorageInventory) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = state.join(format!(".storage-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        serde_json::to_writer(
            &mut file,
            &Cache {
                version: 1,
                inventory: inventory.clone(),
            },
        )?;
        file.flush()?;
        file.sync_all()?;
        std::fs::rename(&temporary, state.join("storage.json"))?;
        File::open(state)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

async fn collect_cached(
    runner: impl Runner,
    state: &Path,
    managed_pool: &str,
    now: u64,
) -> Result<StorageInventory> {
    use std::os::unix::fs::OpenOptionsExt;
    let lock_path = state.join("storage.lock");
    let _lock = tokio::task::spawn_blocking(move || {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(lock_path)?;
        lock.lock()?;
        Ok::<_, anyhow::Error>(lock)
    })
    .await
    .context("Storage cache lock failed")??;
    let mut inventory = observe(runner, managed_pool, load_cache(state), now, SMART_BATCH).await;
    if let Err(error) = save_cache(state, &inventory) {
        inventory.warnings.push(format!(
            "Storage cache could not be persisted: {}",
            error.to_string().chars().take(256).collect::<String>()
        ));
    }
    Ok(inventory)
}

pub async fn collect() -> Result<StorageInventory> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    ensure!(
        cfg!(target_os = "linux") && nix::unistd::Uid::effective().is_root(),
        "Storage collection requires the local root Linux host adapter"
    );
    let config = Path::new("/etc/homecloud/host.json");
    let metadata = std::fs::metadata(config)?;
    ensure!(
        metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
        "Host config must be owned and writable only by root"
    );
    #[derive(Deserialize)]
    struct Policy {
        pool: String,
    }
    let policy: Policy = serde_json::from_reader(File::open(config)?.take(1024 * 1024))?;
    ensure!(
        !policy.pool.is_empty()
            && policy
                .pool
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "Invalid managed pool name"
    );
    let state = Path::new("/var/lib/homecloud/host");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)?;
    let metadata = std::fs::symlink_metadata(state)?;
    ensure!(
        metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
        "Storage state must be a root-owned directory writable only by root"
    );
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    collect_cached(SystemRunner, state, &policy.pool, now).await
}
