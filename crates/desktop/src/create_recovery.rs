//! One unresolved Create per paired profile. No credentials are stored here.
use anyhow::{Context, Result, ensure};
use homecloud_client::private_write;
use homecloud_core::{Mutation, Request, VmSpec};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Binding {
    pub server: String,
    pub device: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PendingCreate {
    version: u32,
    binding: Binding,
    pub key: Uuid,
    pub spec: VmSpec,
}
impl PendingCreate {
    pub fn request(&self) -> Request {
        Request::CreateVm {
            idempotency_key: self.key,
            spec: self.spec.clone(),
        }
    }
    pub fn matches(&self, mutation: &Mutation) -> bool {
        serde_json::to_value(mutation).ok()
            == serde_json::to_value(Mutation::Create {
                spec: self.spec.clone(),
            })
            .ok()
    }
}

pub(super) struct Outbox {
    path: PathBuf,
    binding: Binding,
    _lock: File,
}
fn sibling(profile: &Path, suffix: &str) -> PathBuf {
    let mut name = profile.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}
impl Outbox {
    pub fn open(profile: &Path, binding: Binding) -> Result<Self> {
        let path = sibling(profile, ".create-request.json");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(sibling(profile, ".create-request.lock"))?;
        lock.try_lock().context(
            "Another Homecloud window is managing VM creation for this profile; use that window",
        )?;
        let out = Self {
            path,
            binding,
            _lock: lock,
        };
        out.load()?;
        Ok(out)
    }
    pub fn load(&self) -> Result<Option<PendingCreate>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        ensure!(
            bytes.len() <= homecloud_core::MAX_FRAME,
            "Saved Create request exceeds limit"
        );
        let pending: PendingCreate = serde_json::from_slice(&bytes)
            .context("Cannot read the saved Create request; it has been kept for recovery")?;
        ensure!(
            pending.version == 1,
            "Unsupported saved Create request version"
        );
        ensure!(
            pending.binding == self.binding,
            "Saved Create belongs to another host or device; restore the original paired profile to recover it"
        );
        pending.spec.resources(0).map_err(anyhow::Error::msg)?;
        Ok(Some(pending))
    }
    pub fn prepare(&self, spec: VmSpec) -> Result<PendingCreate> {
        ensure!(
            self.load()?.is_none(),
            "Resolve the saved Create request before submitting another VM"
        );
        spec.resources(0).map_err(anyhow::Error::msg)?;
        let pending = PendingCreate {
            version: 1,
            binding: self.binding.clone(),
            key: Uuid::new_v4(),
            spec,
        };
        private_write(&self.path, &serde_json::to_vec_pretty(&pending)?)?;
        Ok(pending)
    }
    pub fn resolve(&self, key: Uuid) -> Result<()> {
        if let Some(pending) = self.load()? {
            ensure!(
                pending.key == key,
                "Saved Create request changed; keep it for recovery"
            );
            std::fs::remove_file(&self.path)?;
        }
        if let Some(parent) = self.path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}
