use anyhow::{Context, Result, ensure};
use homecloud_core::*;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub capacity: Resources,
    pub core_groups: Vec<Vec<u32>>,
    pub disk_overhead_gib_per_vm: u64,
}
#[derive(Clone)]
pub struct Store {
    path: Arc<PathBuf>,
    limits: Limits,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn json(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
fn parse<T: serde::de::DeserializeOwned>(value: String) -> Result<T> {
    Ok(serde_json::from_str(&value)?)
}
fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}
fn new_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("Secure random generator: {e}"))?;
    Ok(hex::encode(bytes))
}
fn role_name(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::User => "user",
    }
}

impl Store {
    pub fn open(path: impl AsRef<Path>, limits: Limits) -> Result<Self> {
        ensure!(
            limits.core_groups.len() as u64 == limits.capacity.cpu_cores,
            "CPU capacity must equal complete core groups"
        );
        let mut ids = std::collections::HashSet::new();
        ensure!(
            limits
                .core_groups
                .iter()
                .all(|g| !g.is_empty() && g.iter().all(|id| ids.insert(*id))),
            "CPU groups overlap or are empty"
        );
        let store = Self {
            path: Arc::new(path.as_ref().to_path_buf()),
            limits,
        };
        if let Some(parent) = store.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = store.connect()?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(include_str!("schema.sql"))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key='limits'", [], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(old) = old {
            ensure!(
                parse::<Limits>(old)? == store.limits,
                "Host limits changed; explicit migration required before issuing resources"
            );
        } else {
            tx.execute(
                "INSERT INTO meta VALUES ('limits',?)",
                [json(&store.limits)?],
            )?;
        }
        tx.commit()?;
        Ok(store)
    }
    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&*self.path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        Ok(conn)
    }
    fn transaction<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn limits(&self) -> &Limits {
        &self.limits
    }
    pub fn audit_failure(&self, peer: &str) -> Result<()> {
        audit(&self.connect()?, peer, "failed_control_request", "")
    }
    pub fn observe(&self, id: Uuid, state: String, error: Option<String>) -> Result<()> {
        self.transaction(|tx| {
            let count:i64=tx.query_row("SELECT count(*) FROM operations WHERE vm_id=? AND phase NOT IN ('succeeded','failed')",[id.to_string()],|r|r.get(0))?;
            if count!=0{return Ok(())}
            let mut vm=get_vm(tx,id)?;
            if vm.observed=="deleted"{return Ok(())}
            if vm.observed!=state||vm.error!=error {
                vm.observed=state;vm.error=error;
                tx.execute("UPDATE vms SET record=? WHERE id=?",params![json(&vm)?,id.to_string()])?;
            }
            Ok(())
        })
    }
    pub fn bootstrap(&self, name: &str) -> Result<(User, String)> {
        self.transaction(|tx| {
            let count: i64 =
                tx.query_row("SELECT count(*) FROM users WHERE role='admin'", [], |r| {
                    r.get(0)
                })?;
            ensure!(
                count == 0,
                "Bootstrap is closed; use local recover-admin for lost devices"
            );
            let user = insert_user(tx, name, Role::Admin, Resources::default(), true)?;
            let (token, _) = insert_invitation(tx, user.id, 1200)?;
            audit(tx, "local", "bootstrap_admin", &user.id.to_string())?;
            Ok((user, token))
        })
    }
    pub fn recover_admin(&self, user_id: Uuid) -> Result<String> {
        self.transaction(|tx| {
            let user = get_user(tx, user_id)?;
            ensure!(
                user.role == Role::Admin,
                "Recovery target must be an administrator"
            );
            tx.execute(
                "UPDATE users SET active=1 WHERE id=?",
                [user_id.to_string()],
            )?;
            let (token, _) = insert_invitation(tx, user_id, 1200)?;
            audit(tx, "local", "recover_admin_device", &user_id.to_string())?;
            Ok(token)
        })
    }
    pub fn enroll(&self, token: &str, peer: &str) -> Result<User> {
        ensure!(
            token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid or expired invitation"
        );
        self.transaction(|tx| {
            let row: Option<(String, i64)> = tx.query_row("SELECT user_id,expires_at FROM invitations WHERE verifier=? AND consumed_at IS NULL", [token_hash(token)], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            let (user_id, expiry) = row.context("Invalid or expired invitation")?;
            ensure!(expiry > 0 && now() < expiry as u64, "Invalid or expired invitation");
            let user = get_user(tx, user_id.parse()?)?;
            ensure!(user.active, "Account disabled");
            ensure!(tx.query_row("SELECT count(*) FROM devices WHERE peer=?", [peer], |r| r.get::<_, i64>(0))? == 0, "Device already registered or revoked; use a fresh key");
            tx.execute("INSERT INTO devices(peer,user_id,active) VALUES (?,?,1)", params![peer, user_id])?;
            ensure!(tx.execute("UPDATE invitations SET consumed_at=? WHERE verifier=? AND consumed_at IS NULL", params![i64::try_from(now())?, token_hash(token)])? == 1, "Invitation already redeemed");
            audit(tx, peer, "enroll_device", &user.id.to_string())?;
            Ok(user)
        })
    }
    pub fn authenticate(&self, peer: &str) -> Result<User> {
        actor(&self.connect()?, peer)
    }
    pub fn invite(
        &self,
        peer: &str,
        name: &str,
        role: Role,
        grant: Resources,
        gpu_allowed: bool,
        hours: u64,
    ) -> Result<(String, Uuid, u64)> {
        ensure!(
            (1..=168).contains(&hours),
            "Invitation expiry must be 1–168 hours"
        );
        self.transaction(|tx| {
            admin(tx, peer)?;
            let user = insert_user(tx, name, role, grant, gpu_allowed)?;
            self.check_host_promises(tx)?;
            let (token, expiry) = insert_invitation(tx, user.id, hours * 3600)?;
            audit(tx, peer, "invite_user", &user.id.to_string())?;
            Ok((token, user.id, expiry))
        })
    }
    pub fn invite_device(&self, peer: &str, user_id: Uuid, hours: u64) -> Result<(String, u64)> {
        ensure!(
            (1..=168).contains(&hours),
            "Invitation expiry must be 1–168 hours"
        );
        self.transaction(|tx| {
            let me = actor(tx, peer)?;
            ensure!(me.id == user_id || me.role == Role::Admin, "Forbidden");
            ensure!(get_user(tx, user_id)?.active, "Account disabled");
            let result = insert_invitation(tx, user_id, hours * 3600)?;
            audit(tx, peer, "invite_device", &user_id.to_string())?;
            Ok(result)
        })
    }
    pub fn grant(
        &self,
        peer: &str,
        user_id: Uuid,
        grant: Resources,
        gpu_allowed: bool,
    ) -> Result<()> {
        self.transaction(|tx| {
            admin(tx, peer)?;
            get_user(tx, user_id)?;
            tx.execute(
                "UPDATE users SET grant_json=?,gpu_allowed=? WHERE id=?",
                params![json(&grant)?, gpu_allowed, user_id.to_string()],
            )?;
            self.check_host_promises(tx)?;
            audit(tx, peer, "set_grant", &user_id.to_string())
        })
    }
    pub fn revoke_device(&self, peer: &str, target: &str) -> Result<()> {
        self.transaction(|tx| {
            let me = actor(tx, peer)?;
            let owner: String =
                tx.query_row("SELECT user_id FROM devices WHERE peer=?", [target], |r| {
                    r.get(0)
                })?;
            ensure!(
                owner == me.id.to_string() || me.role == Role::Admin,
                "Forbidden"
            );
            tx.execute("UPDATE devices SET active=0 WHERE peer=?", [target])?;
            audit(tx, peer, "revoke_device", target)
        })
    }
    pub fn set_account_active(&self, peer: &str, target: Uuid, active: bool) -> Result<()> {
        self.transaction(|tx| {
            let me = admin(tx, peer)?;
            ensure!(
                target != me.id || active,
                "Use another administrator to disable your own account"
            );
            ensure!(
                tx.execute(
                    "UPDATE users SET active=? WHERE id=?",
                    params![active, target.to_string()]
                )? == 1,
                "User not found"
            );
            audit(tx, peer, "set_account_active", &target.to_string())
        })
    }
    fn check_host_promises(&self, conn: &Connection) -> Result<()> {
        let mut total = Resources::default();
        for user in users(conn)? {
            total = total
                .checked_add(user.grant.max(user.committed))
                .map_err(anyhow::Error::msg)?;
        }
        Resources::default()
            .admit(total, self.limits.capacity)
            .map_err(anyhow::Error::msg)
    }
    pub fn users(&self) -> Result<Vec<User>> {
        users(&self.connect()?)
    }
    pub fn devices(&self, peer: &str) -> Result<Vec<Device>> {
        let conn = self.connect()?;
        let me = actor(&conn, peer)?;
        let mut stmt = conn.prepare("SELECT peer,user_id,active FROM devices")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })?;
        let mut result = vec![];
        for row in rows {
            let (peer, user, active) = row?;
            let user_id = user.parse()?;
            if me.role == Role::Admin || me.id == user_id {
                result.push(Device {
                    peer,
                    user_id,
                    active,
                });
            }
        }
        Ok(result)
    }
    pub fn vms(&self) -> Result<Vec<Vm>> {
        vms(&self.connect()?)
    }
    pub fn vm(&self, id: Uuid) -> Result<Vm> {
        get_vm(&self.connect()?, id)
    }
    pub fn owned_vm(&self, peer: &str, id: Uuid) -> Result<Vm> {
        let conn = self.connect()?;
        let me = actor(&conn, peer)?;
        let vm = get_vm(&conn, id)?;
        ensure!(me.role == Role::Admin || me.id == vm.owner, "Forbidden");
        ensure!(vm.observed != "deleted", "VM deleted");
        Ok(vm)
    }
    pub fn submit(&self, peer: &str, key: Uuid, request: Mutation) -> Result<Operation> {
        self.transaction(|tx| {
            let me = actor(tx, peer)?;
            let request_json = json(&request)?;
            if let Some(old) = tx.query_row("SELECT record FROM operations WHERE actor=? AND request_key=?", params![me.id.to_string(),key.to_string()], |r| r.get::<_,String>(0)).optional()? {
                let op: Operation = parse(old)?;
                ensure!(json(&op.request)? == request_json, "Idempotency key reused for a different request");
                return Ok(op);
            }
            let mut vm = match &request {
                Mutation::Create { spec } => {
                    let resources = spec.resources(self.limits.disk_overhead_gib_per_vm).map_err(anyhow::Error::msg)?;
                    let after = me.committed.checked_add(resources).map_err(anyhow::Error::msg)?;
                    me.committed.admit(after, me.grant).map_err(anyhow::Error::msg)?;
                    let id = Uuid::new_v4();
                    let slot = (1..=200).find(|slot| tx.query_row("SELECT count(*) FROM network_leases WHERE slot=?", [slot], |r| r.get::<_,i64>(0)).unwrap_or(1) == 0).context("No free private network slots")?;
                    let cpu_ids = self.allocate_cores(tx, id, spec.cores)?;
                    tx.execute("INSERT INTO network_leases VALUES (?,?)", params![slot,id.to_string()])?;
                    Vm { id, owner: me.id, spec: spec.clone(), resources, cpu_ids, network_slot: slot as u16, desired: Lifecycle::Running, observed: "pending".into(), ssh_host_key: None, gpu: None, error: None }
                }
                other => {
                    let id = match other { Mutation::State { vm_id, .. } | Mutation::Resize { vm_id, .. } | Mutation::Gpu { vm_id, .. } => *vm_id, _ => unreachable!() };
                    let vm = get_vm(tx, id)?;
                    ensure!(vm.owner == me.id || me.role == Role::Admin, "Forbidden");
                    ensure!(vm.desired != Lifecycle::Deleted && vm.observed != "deleted", "VM is being deleted or was deleted");
                    let count: i64 = tx.query_row("SELECT count(*) FROM operations WHERE vm_id=? AND phase NOT IN ('succeeded','failed')", [id.to_string()], |r| r.get(0))?;
                    ensure!(count == 0, "VM already has an unfinished operation");
                    vm
                }
            };
            match &request {
                Mutation::State { desired, .. } => vm.desired = *desired,
                Mutation::Resize { memory_gib, cores, disk_gib, .. } => {
                    ensure!(*disk_gib >= vm.spec.disk_gib, "Shrinking VM disks is unsupported");
                    let spec = VmSpec { memory_gib: *memory_gib, cores: *cores, disk_gib: *disk_gib, ..vm.spec.clone() };
                    let wanted = spec.resources(self.limits.disk_overhead_gib_per_vm).map_err(anyhow::Error::msg)?;
                    let owner = get_user(tx, vm.owner)?;
                    let after = subtract(owner.committed, vm.resources)?.checked_add(wanted).map_err(anyhow::Error::msg)?;
                    owner.committed.admit(after, owner.grant).map_err(anyhow::Error::msg)?;
                    if *cores > vm.spec.cores { vm.cpu_ids.extend(self.allocate_cores(tx, vm.id, *cores - vm.spec.cores)?); }
                    else { vm.cpu_ids = self.limits.core_groups.iter().filter(|group| group.iter().all(|x| vm.cpu_ids.contains(x))).take(*cores as usize).flatten().copied().collect(); }
                    vm.resources = vm.resources.max(wanted); // retain old backing until host confirmation
                    vm.spec = spec;
                }
                Mutation::Gpu { pci, .. } => {
                    if let Some(pci) = pci {
                        validate_pci(pci)?;
                        ensure!(get_user(tx, vm.owner)?.gpu_allowed, "GPU permission denied");
                        ensure!(vm.spec.memory_gib >= 4, "GPU VMs require at least 4 GiB RAM on the initial host");
                        ensure!(vm.gpu.is_none(), "Detach the current GPU first");
                        tx.execute("INSERT INTO gpu_leases(pci,vm_id) VALUES (?,?)", params![pci, vm.id.to_string()]).context("GPU is already reserved")?;
                    }
                    // Retain the old lease during detach until the host confirms release.
                    vm.gpu = pci.clone();
                }
                _ => {}
            }
            tx.execute("INSERT INTO vms(id,owner,record) VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET record=excluded.record", params![vm.id.to_string(),vm.owner.to_string(),json(&vm)?])?;
            self.check_host_promises(tx)?;
            let op = Operation { id: Uuid::new_v4(), vm_id: vm.id, actor: me.id, request, phase: "pending".into(), error: None };
            tx.execute("INSERT INTO operations(id,actor,request_key,vm_id,phase,record) VALUES (?,?,?,?,?,?)", params![op.id.to_string(),me.id.to_string(),key.to_string(),vm.id.to_string(),op.phase,json(&op)?])?;
            audit(tx, peer, "request_operation", &op.id.to_string())?;
            Ok(op)
        })
    }
    fn allocate_cores(&self, tx: &Transaction<'_>, vm: Uuid, count: u64) -> Result<Vec<u32>> {
        let mut free = vec![];
        for group in &self.limits.core_groups {
            if group.iter().all(|id| {
                tx.query_row(
                    "SELECT count(*) FROM cpu_leases WHERE cpu_id=?",
                    [id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(1)
                    == 0
            }) {
                free.push(group);
            }
        }
        ensure!(free.len() >= count as usize, "No free physical cores");
        let selected: Vec<u32> = free
            .into_iter()
            .take(count as usize)
            .flatten()
            .copied()
            .collect();
        for cpu in &selected {
            tx.execute(
                "INSERT INTO cpu_leases VALUES (?,?)",
                params![cpu, vm.to_string()],
            )?;
        }
        Ok(selected)
    }
    pub fn operations(&self) -> Result<Vec<Operation>> {
        let conn = self.connect()?;
        let mut stmt =
            conn.prepare("SELECT record FROM operations ORDER BY rowid DESC LIMIT 200")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| parse(r?))
            .collect()
    }
    pub fn retry_operation(&self, peer: &str, id: Uuid) -> Result<()> {
        self.transaction(|tx| {
            admin(tx, peer)?;
            let mut op: Operation = parse(tx.query_row(
                "SELECT record FROM operations WHERE id=?",
                [id.to_string()],
                |r| r.get(0),
            )?)?;
            ensure!(
                op.phase == "needs_review",
                "Only operations awaiting review need an explicit retry"
            );
            op.phase = "pending".into();
            op.error = None;
            tx.execute(
                "UPDATE operations SET phase='pending',record=? WHERE id=?",
                params![json(&op)?, id.to_string()],
            )?;
            audit(tx, peer, "retry_reviewed_operation", &id.to_string())
        })
    }
    pub fn pending(&self) -> Result<Vec<Operation>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare("SELECT record FROM operations WHERE phase IN ('pending','applying','retrying') ORDER BY rowid")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| parse(r?))
            .collect()
    }
    pub fn operation_phase(&self, id: Uuid, phase: &str, error: Option<String>) -> Result<()> {
        self.transaction(|tx| {
            let mut op: Operation = parse(tx.query_row(
                "SELECT record FROM operations WHERE id=?",
                [id.to_string()],
                |r| r.get(0),
            )?)?;
            op.phase = phase.into();
            op.error = error.clone();
            tx.execute(
                "UPDATE operations SET phase=?,record=? WHERE id=?",
                params![phase, json(&op)?, id.to_string()],
            )?;
            let mut vm = get_vm(tx, op.vm_id)?;
            vm.error = error;
            tx.execute(
                "UPDATE vms SET record=? WHERE id=?",
                params![json(&vm)?, vm.id.to_string()],
            )?;
            Ok(())
        })
    }
    pub fn complete(
        &self,
        op_id: Uuid,
        observed: String,
        ssh_host_key: Option<String>,
    ) -> Result<()> {
        self.transaction(|tx| {
            let mut op: Operation = parse(tx.query_row(
                "SELECT record FROM operations WHERE id=?",
                [op_id.to_string()],
                |r| r.get(0),
            )?)?;
            let mut vm = get_vm(tx, op.vm_id)?;
            vm.observed = observed;
            vm.error = None;
            if ssh_host_key.is_some() {
                vm.ssh_host_key = ssh_host_key;
            }
            if vm.desired == Lifecycle::Deleted {
                ensure!(vm.observed == "deleted", "Deletion not confirmed by host");
                for table in ["cpu_leases", "gpu_leases", "network_leases"] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE vm_id=?"),
                        [vm.id.to_string()],
                    )?;
                }
            } else {
                vm.resources = vm
                    .spec
                    .resources(self.limits.disk_overhead_gib_per_vm)
                    .map_err(anyhow::Error::msg)?;
                let mut stmt = tx.prepare("SELECT cpu_id FROM cpu_leases WHERE vm_id=?")?;
                let current: Vec<u32> = stmt
                    .query_map([vm.id.to_string()], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for cpu in current {
                    if !vm.cpu_ids.contains(&cpu) {
                        tx.execute("DELETE FROM cpu_leases WHERE cpu_id=?", [cpu])?;
                    }
                }
                if vm.gpu.is_none() {
                    tx.execute("DELETE FROM gpu_leases WHERE vm_id=?", [vm.id.to_string()])?;
                }
            }
            tx.execute(
                "UPDATE vms SET record=? WHERE id=?",
                params![json(&vm)?, vm.id.to_string()],
            )?;
            op.phase = "succeeded".into();
            op.error = None;
            tx.execute(
                "UPDATE operations SET phase='succeeded',record=? WHERE id=?",
                params![json(&op)?, op_id.to_string()],
            )?;
            audit(tx, "reconciler", "operation_succeeded", &op.id.to_string())
        })
    }
}

fn actor(conn: &Connection, peer: &str) -> Result<User> {
    let user: Option<String> = conn
        .query_row(
            "SELECT user_id FROM devices WHERE peer=? AND active=1",
            [peer],
            |r| r.get(0),
        )
        .optional()?;
    let user = get_user(conn, user.context("Device is not authorized")?.parse()?)?;
    ensure!(user.active, "Account disabled");
    Ok(user)
}
fn admin(conn: &Connection, peer: &str) -> Result<User> {
    let user = actor(conn, peer)?;
    ensure!(
        user.role == Role::Admin,
        "Administrator permission required"
    );
    Ok(user)
}
fn audit(conn: &Connection, actor: &str, action: &str, target: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO audit(timestamp,actor,action,target) VALUES (?,?,?,?)",
        params![i64::try_from(now())?, actor, action, target],
    )?;
    Ok(())
}
fn insert_user(
    tx: &Transaction<'_>,
    name: &str,
    role: Role,
    grant: Resources,
    gpu: bool,
) -> Result<User> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 80 && !name.chars().any(char::is_control),
        "Invalid display name"
    );
    let id = Uuid::new_v4();
    tx.execute(
        "INSERT INTO users VALUES (?,?,?,?,?,?)",
        params![
            id.to_string(),
            name,
            role_name(role),
            true,
            json(&grant)?,
            gpu
        ],
    )?;
    get_user(tx, id)
}
fn insert_invitation(tx: &Transaction<'_>, user: Uuid, ttl: u64) -> Result<(String, u64)> {
    let token = new_token()?;
    let expiry = now().checked_add(ttl).context("Invalid expiry")?;
    tx.execute(
        "INSERT INTO invitations VALUES (?,?,?,NULL)",
        params![token_hash(&token), user.to_string(), i64::try_from(expiry)?],
    )?;
    Ok((token, expiry))
}
fn get_user(conn: &Connection, id: Uuid) -> Result<User> {
    let (name, role, active, grant, gpu): (String, String, bool, String, bool) = conn.query_row(
        "SELECT name,role,active,grant_json,gpu_allowed FROM users WHERE id=?",
        [id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let mut committed = Resources::default();
    for vm in vms(conn)?
        .into_iter()
        .filter(|v| v.owner == id && v.observed != "deleted")
    {
        committed = committed
            .checked_add(vm.resources)
            .map_err(anyhow::Error::msg)?;
    }
    Ok(User {
        id,
        name,
        role: if role == "admin" {
            Role::Admin
        } else {
            Role::User
        },
        active,
        grant: parse(grant)?,
        committed,
        gpu_allowed: gpu,
    })
}
fn users(conn: &Connection) -> Result<Vec<User>> {
    let mut stmt = conn.prepare("SELECT id FROM users ORDER BY rowid")?;
    stmt.query_map([], |r| r.get::<_, String>(0))?
        .map(|r| get_user(conn, r?.parse()?))
        .collect()
}
fn vms(conn: &Connection) -> Result<Vec<Vm>> {
    let mut stmt = conn.prepare("SELECT record FROM vms ORDER BY rowid")?;
    stmt.query_map([], |r| r.get::<_, String>(0))?
        .map(|r| parse(r?))
        .collect()
}
fn get_vm(conn: &Connection, id: Uuid) -> Result<Vm> {
    parse(
        conn.query_row("SELECT record FROM vms WHERE id=?", [id.to_string()], |r| {
            r.get(0)
        })?,
    )
}
fn subtract(a: Resources, b: Resources) -> Result<Resources> {
    Ok(Resources {
        memory_bytes: a
            .memory_bytes
            .checked_sub(b.memory_bytes)
            .context("Memory accounting mismatch")?,
        cpu_cores: a
            .cpu_cores
            .checked_sub(b.cpu_cores)
            .context("CPU accounting mismatch")?,
        disk_bytes: a
            .disk_bytes
            .checked_sub(b.disk_bytes)
            .context("Disk accounting mismatch")?,
    })
}
pub fn validate_pci(pci: &str) -> Result<()> {
    ensure!(
        pci.len() == 12
            && pci.bytes().enumerate().all(|(i, c)| match i {
                4 | 7 => c == b':',
                10 => c == b'.',
                _ => c.is_ascii_hexdigit(),
            }),
        "Invalid PCI address"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
