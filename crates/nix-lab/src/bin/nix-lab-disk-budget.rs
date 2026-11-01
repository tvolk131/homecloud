//! Offline, backed, disk-only policy migration for this lab proof.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};
const GIB: u64 = 1024 * 1024 * 1024;
fn command(program: &str, args: &[&str]) -> Result<String> {
    let p = Command::new(program).args(args).output()?;
    ensure!(
        p.status.success(),
        "{program} failed: {}",
        String::from_utf8_lossy(&p.stderr)
    );
    Ok(String::from_utf8(p.stdout)?)
}
fn write(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let metadata = fs::metadata(path)?;
    let mode = metadata.mode() & 0o777;
    let tmp = path.with_extension("nix-lab-stage");
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(&tmp)?;
    file.write_all(data)?;
    std::os::unix::fs::chown(&tmp, Some(metadata.uid()), Some(metadata.gid()))?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
fn disk_promises(conn: &Connection) -> Result<u64> {
    Ok(conn.query_row("SELECT coalesce(sum(max(json_extract(grant_json,'$.disk_bytes'),coalesce((SELECT sum(json_extract(record,'$.resources.disk_bytes')) FROM vms WHERE owner=users.id),0))),0) FROM users", [], |row| row.get::<_, i64>(0))?.try_into()?)
}
fn migrate(backup: &Path, target: u64) -> Result<()> {
    let _controller = fs::OpenOptions::new()
        .create(false)
        .read(true)
        .write(true)
        .open("/var/lib/homecloud/control/state.db.controller.lock")?;
    _controller
        .try_lock()
        .context("Controller still owns its database")?;
    let _host = fs::OpenOptions::new()
        .create(false)
        .read(true)
        .write(true)
        .open("/var/lib/homecloud/host/operations.lock")?;
    _host.try_lock().context("A host operation is active")?;
    let host_path = Path::new("/etc/homecloud/host.json");
    let control_path = Path::new("/etc/homecloud/control.json");
    let old_host = fs::read(host_path)?;
    let old_control = fs::read(control_path)?;
    let mut host: Value = serde_json::from_slice(&old_host)?;
    let mut control: Value = serde_json::from_slice(&old_control)?;
    let mut conn = Connection::open("/var/lib/homecloud/control/state.db")?;
    let old_limits: String =
        conn.query_row("SELECT value FROM meta WHERE key='limits'", [], |r| {
            r.get(0)
        })?;
    let mut limits: Value = serde_json::from_str(&old_limits)?;
    ensure!(
        limits == control["limits"],
        "Controller config and persisted baseline disagree"
    );
    let old = limits["capacity"]["disk_bytes"]
        .as_u64()
        .context("Disk capacity")?;
    ensure!(
        host["capacity"]["disk_bytes"] == old,
        "Host disk baseline disagrees"
    );
    ensure!(
        target != old && target.is_multiple_of(GIB),
        "Choose a different whole-GiB budget"
    );
    for query in [
        "SELECT count(*) FROM operations WHERE phase NOT IN ('succeeded','failed')",
        "SELECT count(*) FROM host_reserve_operations WHERE phase NOT IN ('succeeded','cancelled')",
        "SELECT count(*) FROM memory_capacity_operations WHERE terminal=0",
        "SELECT count(*) FROM host_shutdown_operations WHERE active=1",
        "SELECT count(*) FROM application_operations WHERE phase IN ('pending','running','waiting_for_user')",
    ] {
        ensure!(
            conn.query_row(query, [], |r| r.get::<_, i64>(0))? == 0,
            "Pending operation prevents offline migration"
        );
    }
    let reserves: Value = serde_json::from_str(&conn.query_row(
        "SELECT value FROM meta WHERE key='host_reserves'",
        [],
        |r| r.get::<_, String>(0),
    )?)?;
    let disk_reserve = reserves["additional"]["disk_bytes"]
        .as_u64()
        .context("Reserve disk budget")?;
    let promised = disk_promises(&conn)?;
    ensure!(
        target
            >= promised
                .checked_add(disk_reserve)
                .context("Disk promises overflow")?,
        "New disk budget would undercut promises"
    );
    let pool = host["pool"].as_str().context("Pool")?;
    let available: u64 = command("zfs", &["list", "-Hp", "-o", "available", pool])?
        .trim()
        .parse()?;
    let floor = host["minimum_pool_available_bytes"]
        .as_u64()
        .context("Pool floor")?
        + disk_reserve;
    ensure!(
        available >= target.saturating_sub(old) + floor + 16 * GIB,
        "Insufficient physical headroom, including prototype store"
    );
    fs::create_dir(backup).context("Backup directory must be new")?;
    command("chmod", &["0700", backup.to_str().unwrap()])?;
    fs::write(backup.join("host.json"), &old_host)?;
    fs::write(backup.join("control.json"), &old_control)?;
    fs::write(backup.join("limits.json"), &old_limits)?;
    conn.execute("VACUUM INTO ?", [backup.join("state.db").to_str().unwrap()])?;
    host["capacity"]["disk_bytes"] = json!(target);
    control["limits"]["capacity"]["disk_bytes"] = json!(target);
    limits["capacity"]["disk_bytes"] = json!(target);
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE meta SET value=? WHERE key='limits'",
        [serde_json::to_string(&limits)?],
    )?;
    let files = (|| -> Result<()> {
        write(host_path, &serde_json::to_vec_pretty(&host)?)?;
        write(control_path, &serde_json::to_vec_pretty(&control)?)?;
        Ok(())
    })();
    if let Err(error) = files {
        write(host_path, &old_host)?;
        write(control_path, &old_control)?;
        return Err(error);
    }
    if let Err(error) = tx.commit() {
        write(host_path, &old_host)?;
        write(control_path, &old_control)?;
        return Err(error.into());
    }
    fs::write(
        backup.join("receipt.json"),
        serde_json::to_vec_pretty(
            &json!({"old_bytes":old,"new_bytes":target,"promised_bytes":promised,"available_before":available,"vm_records_changed":false}),
        )?,
    )?;
    println!(
        "Disk baseline: {} -> {} GiB; grants and VM claims preserved",
        old / GIB,
        target / GIB
    );
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 3,
        "Usage: nix-lab-disk-budget NEW_GIB NEW_BACKUP_DIRECTORY"
    );
    let target = args[1]
        .parse::<u64>()?
        .checked_mul(GIB)
        .context("Disk budget overflow")?;
    let backup = PathBuf::from(&args[2]);
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open("/var/lib/homecloud/nix-lab-budget.lock")?;
    lock.try_lock()?;
    command("systemctl", &["stop", "homecloudd"])?;
    let result = migrate(&backup, target);
    // A caught failure restores config files above. A process crash stays fail-closed.
    let start = command("systemctl", &["start", "homecloudd"]);
    result?;
    start?;
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        command("systemctl", &["is-active", "homecloudd"])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_owners_keep_their_grants_and_vm_claims() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE users(id TEXT, active INTEGER, grant_json TEXT); CREATE TABLE vms(owner TEXT, record TEXT);")
            .unwrap();
        assert_eq!(disk_promises(&conn).unwrap(), 0);
        for (owner, active, grant) in [("enabled", true, 64 * GIB), ("disabled", false, 32 * GIB)] {
            conn.execute(
                "INSERT INTO users VALUES (?,?,?)",
                rusqlite::params![owner, active, json!({"disk_bytes":grant}).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO vms VALUES (?,?)",
            rusqlite::params![
                "disabled",
                json!({"resources":{"disk_bytes":96 * GIB}}).to_string()
            ],
        )
        .unwrap();
        assert_eq!(disk_promises(&conn).unwrap(), 160 * GIB);
    }

    #[test]
    fn config_write_child() {
        if let Some(path) = std::env::var_os("HOMECLOUD_NIX_LAB_CONFIG_TEST") {
            write(Path::new(&path), b"replacement").unwrap();
        }
    }

    #[test]
    fn config_permissions_survive_a_restrictive_umask() {
        let directory = std::env::temp_dir().join(format!("hcnix-mode-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("control.json");
        fs::write(&path, b"original").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let original = fs::metadata(&path).unwrap();
        let status = Command::new("sh")
            .args(["-c", "umask 077; exec \"$@\"", "config-test"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "tests::config_write_child"])
            .env("HOMECLOUD_NIX_LAB_CONFIG_TEST", &path)
            .status()
            .unwrap();
        assert!(status.success());
        let replacement = fs::metadata(&path).unwrap();
        assert_eq!(replacement.mode() & 0o777, 0o640);
        assert_eq!(replacement.uid(), original.uid());
        assert_eq!(replacement.gid(), original.gid());
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        fs::remove_dir_all(directory).unwrap();
    }
}
