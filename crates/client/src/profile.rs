//! Public pairing metadata stays on disk; macOS device keys live in Keychain.
use anyhow::{Context, Result, ensure};
use iroh::{EndpointAddr, SecretKey};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::Path,
};

// Deliberately not Debug/Serialize: an in-memory profile contains a device key.
#[derive(Clone)]
pub struct Profile {
    pub server: EndpointAddr,
    pub device_secret: String,
}

/// Storage boundary used by native Keychain and deterministic transport tests.
/// An unavailable/locked store is an error, never a plaintext fallback.
pub trait CredentialStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>>;
    fn put(&self, account: &str, secret: &[u8]) -> Result<()>;
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProfile {
    server: EndpointAddr,
    device_secret: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecureProfile {
    version: u32,
    server: EndpointAddr,
    credential: CredentialReference,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
enum CredentialReference {
    MacosKeychain { account: String, device_id: String },
}
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredProfile {
    Secure(SecureProfile),
    File(FileProfile),
}

fn key(profile: &Profile) -> Result<SecretKey> {
    let bytes: [u8; 32] = hex::decode(&profile.device_secret)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| {
            anyhow::anyhow!("Invalid device credential; recover or enroll this device again")
        })?;
    Ok(SecretKey::from_bytes(&bytes))
}
fn account(server: &EndpointAddr, device_id: &str) -> String {
    format!("{}:{device_id}", server.id)
}
fn read(path: &Path) -> Result<StoredProfile> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .context("No paired host; enroll this device first")?;
    ensure!(
        file.metadata()?.is_file(),
        "Paired host profile must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            file.metadata()?.permissions().mode() & 0o077 == 0,
            "Client profile must be readable only by its owner"
        );
    }
    let mut bytes = vec![];
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "Paired host profile is too large"
    );
    serde_json::from_slice(&bytes).context("Invalid paired host profile")
}
fn lock(path: &Path) -> Result<File> {
    let parent = path.parent().context("Profile has no parent directory")?;
    if !parent.exists() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let name = path
        .file_name()
        .context("Profile has no file name")?
        .to_string_lossy();
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let file = options.open(parent.join(format!("{name}.lock")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            file.metadata()?.permissions().mode() & 0o077 == 0,
            "Profile lock must be private"
        );
    }
    file.lock()?;
    Ok(file)
}
fn same_identity(old: &StoredProfile, profile: &Profile) -> Result<()> {
    let expected = key(profile)?.public().to_string();
    let (server, device) = match old {
        StoredProfile::Secure(p) => {
            ensure!(p.version == 2, "Unsupported paired host profile version");
            let CredentialReference::MacosKeychain { device_id, .. } = &p.credential;
            (&p.server, device_id.clone())
        }
        StoredProfile::File(p) => (
            &p.server,
            key(&Profile {
                server: p.server.clone(),
                device_secret: p.device_secret.clone(),
            })?
            .public()
            .to_string(),
        ),
    };
    ensure!(
        server.id == profile.server.id && device == expected,
        "Profile belongs to another host or device; use a separate profile for new enrollment"
    );
    Ok(())
}
impl Profile {
    pub fn load(path: &Path) -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Self::load_with_store(path, &MacKeychain)
        }
        #[cfg(not(target_os = "macos"))]
        {
            match read(path)? {
                StoredProfile::File(p) => {
                    let profile = Self {
                        server: p.server,
                        device_secret: p.device_secret,
                    };
                    key(&profile)?;
                    Ok(profile)
                }
                StoredProfile::Secure(_) => anyhow::bail!(
                    "This profile uses macOS Keychain; enroll this Linux device separately"
                ),
            }
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.save_with_store(path, &MacKeychain)
        }
        #[cfg(not(target_os = "macos"))]
        {
            key(self)?;
            let _lock = lock(path)?;
            if path.exists() {
                same_identity(&read(path)?, self)?;
            }
            crate::private_write(
                path,
                &serde_json::to_vec_pretty(&FileProfile {
                    server: self.server.clone(),
                    device_secret: self.device_secret.clone(),
                })?,
            )
        }
    }
    pub fn load_with_store(path: &Path, store: &dyn CredentialStore) -> Result<Self> {
        match read(path)? {
            StoredProfile::File(p) => {
                let profile = Self {
                    server: p.server,
                    device_secret: p.device_secret,
                };
                // Re-read under the writer lock. A concurrent migration cannot
                // switch identity or remove plaintext before store verification.
                profile.save_with_store(path, store)?;
                Ok(profile)
            }
            StoredProfile::Secure(p) => {
                ensure!(p.version == 2, "Unsupported paired host profile version");
                let CredentialReference::MacosKeychain {
                    account: stored_account,
                    device_id,
                } = p.credential;
                ensure!(
                    stored_account == account(&p.server, &device_id),
                    "Credential reference does not match the paired host and device"
                );
                let bytes = store.get(&stored_account)?.context(
                    "Device key is missing from Keychain; recover or enroll this device again",
                )?;
                ensure!(
                    bytes.len() == 32,
                    "Stored device key is invalid; recover this device"
                );
                let profile = Self {
                    server: p.server,
                    device_secret: hex::encode(bytes),
                };
                ensure!(
                    key(&profile)?.public().to_string() == device_id,
                    "Stored key does not match this device; recover pairing before connecting"
                );
                Ok(profile)
            }
        }
    }
    pub fn save_with_store(&self, path: &Path, store: &dyn CredentialStore) -> Result<()> {
        let device = key(self)?;
        let _lock = lock(path)?;
        if path.exists() {
            same_identity(&read(path)?, self)?;
        }
        let device_id = device.public().to_string();
        let account = account(&self.server, &device_id);
        let bytes = device.to_bytes();
        match store.get(&account)? {
            Some(old) => ensure!(
                old == bytes,
                "Keychain contains a different device key; recover pairing before saving"
            ),
            None => store.put(&account, &bytes)?,
        }
        ensure!(
            store.get(&account)?.as_deref() == Some(bytes.as_slice()),
            "Keychain did not confirm the device key; the profile was not migrated"
        );
        crate::private_write(
            path,
            &serde_json::to_vec_pretty(&SecureProfile {
                version: 2,
                server: self.server.clone(),
                credential: CredentialReference::MacosKeychain { account, device_id },
            })?,
        )
    }
}

#[cfg(target_os = "macos")]
pub struct MacKeychain;
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "dev.homecloud.device-credentials";
#[cfg(target_os = "macos")]
impl CredentialStore for MacKeychain {
    fn get(&self, account: &str) -> Result<Option<Vec<u8>>> {
        use security_framework::passwords::{PasswordOptions, generic_password};
        match generic_password(PasswordOptions::new_generic_password(
            KEYCHAIN_SERVICE,
            account,
        )) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.code() == -25300 => Ok(None),
            Err(e) => anyhow::bail!(
                "macOS Keychain could not read the Homecloud device key (status {}). Unlock Keychain and allow Homecloud access, then retry.",
                e.code()
            ),
        }
    }
    fn put(&self, account: &str, secret: &[u8]) -> Result<()> {
        security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE,account,secret)
            .map_err(|e|anyhow::anyhow!("macOS Keychain could not save the Homecloud device key (status {}). The existing profile was retained.",e.code()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };
    #[derive(Default)]
    struct MemoryStore {
        keys: Mutex<HashMap<String, Vec<u8>>>,
        writes: AtomicUsize,
    }
    impl CredentialStore for MemoryStore {
        fn get(&self, account: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.keys.lock().unwrap().get(account).cloned())
        }
        fn put(&self, account: &str, secret: &[u8]) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.keys
                .lock()
                .unwrap()
                .insert(account.into(), secret.to_vec());
            Ok(())
        }
    }
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Profile) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        let p = Profile {
            server: EndpointAddr::new(SecretKey::generate().public()),
            device_secret: hex::encode(SecretKey::generate().to_bytes()),
        };
        crate::private_write(
            &path,
            &serde_json::to_vec(&FileProfile {
                server: p.server.clone(),
                device_secret: p.device_secret.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        (dir, path, p)
    }
    #[test]
    fn migration_preserves_identity_and_host_pin_without_plaintext() {
        let (_dir, path, expected) = fixture();
        let store = MemoryStore::default();
        let loaded = Profile::load_with_store(&path, &store).unwrap();
        assert_eq!(loaded.server.id, expected.server.id);
        assert_eq!(loaded.device_secret, expected.device_secret);
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains(&expected.device_secret));
        assert!(!saved.contains("device_secret"));
        assert_eq!(
            Profile::load_with_store(&path, &store)
                .unwrap()
                .device_secret,
            expected.device_secret
        );
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    }
    struct FailedStore {
        stage: usize,
        reads: AtomicUsize,
    }
    impl CredentialStore for FailedStore {
        fn get(&self, _: &str) -> Result<Option<Vec<u8>>> {
            let n = self.reads.fetch_add(1, Ordering::SeqCst);
            if self.stage == 0 || (self.stage == 2 && n > 0) {
                anyhow::bail!("locked")
            };
            Ok(None)
        }
        fn put(&self, _: &str, _: &[u8]) -> Result<()> {
            if self.stage == 1 {
                anyhow::bail!("denied")
            };
            Ok(())
        }
    }
    #[test]
    fn failed_read_write_or_readback_keeps_legacy_profile_recoverable() {
        for stage in 0..4 {
            let (_dir, path, _) = fixture();
            let original = std::fs::read(&path).unwrap();
            assert!(
                Profile::load_with_store(
                    &path,
                    &FailedStore {
                        stage,
                        reads: AtomicUsize::new(0)
                    }
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), original);
            Profile::load_with_store(&path, &MemoryStore::default()).unwrap();
        }
    }
    #[test]
    fn lost_wrong_and_corrupt_keys_never_create_a_new_device() {
        let (_dir, path, _) = fixture();
        let store = MemoryStore::default();
        Profile::load_with_store(&path, &store).unwrap();
        let original = std::fs::read(&path).unwrap();
        let account = store.keys.lock().unwrap().keys().next().unwrap().clone();
        for bytes in [None, Some(vec![1; 31]), Some(vec![1; 32])] {
            store.keys.lock().unwrap().clear();
            if let Some(bytes) = bytes {
                store.keys.lock().unwrap().insert(account.clone(), bytes);
            }
            assert!(Profile::load_with_store(&path, &store).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn tampered_reference_and_future_schema_fail_without_store_effects() {
        let (_dir, path, profile) = fixture();
        let store = MemoryStore::default();
        profile.save_with_store(&path, &store).unwrap();
        let base: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for change in 0..3 {
            let mut json = base.clone();
            match change {
                0 => json["version"] = 3.into(),
                1 => json["credential"]["account"] = "other".into(),
                _ => {
                    json["credential"]["device_id"] =
                        SecretKey::generate().public().to_string().into()
                }
            };
            crate::private_write(&path, &serde_json::to_vec(&json).unwrap()).unwrap();
            assert!(Profile::load_with_store(&path, &store).is_err());
            if change == 0 {
                assert!(profile.save_with_store(&path, &store).is_err());
            }
        }
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn different_host_or_device_cannot_replace_pairing() {
        let (_dir, path, p) = fixture();
        let store = MemoryStore::default();
        for p in [
            Profile {
                server: EndpointAddr::new(SecretKey::generate().public()),
                ..p.clone()
            },
            Profile {
                device_secret: hex::encode(SecretKey::generate().to_bytes()),
                ..p
            },
        ] {
            assert!(p.save_with_store(&path, &store).is_err());
        }
        assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn simultaneous_migration_uses_one_durable_identity() {
        let (_dir, path, p) = fixture();
        let store = Arc::new(MemoryStore::default());
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    let path = &path;
                    let store = &store;
                    scope.spawn(move || {
                        Profile::load_with_store(path, store.as_ref())
                            .unwrap()
                            .device_secret
                    })
                })
                .collect();
            for worker in workers {
                assert_eq!(worker.join().unwrap(), p.device_secret);
            }
        });
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        assert_eq!(store.keys.lock().unwrap().len(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn permissive_or_symlinked_profile_is_rejected_before_migration() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (dir, path, _) = fixture();
        let store = MemoryStore::default();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Profile::load_with_store(&path, &store).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link.json");
        symlink(&path, &link).unwrap();
        assert!(Profile::load_with_store(&link, &store).is_err());
        assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    }
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an unlocked macOS login Keychain; disposable item only"]
    fn native_keychain_roundtrip_and_delete() {
        let account = format!("test-{}", uuid::Uuid::new_v4());
        let bytes = SecretKey::generate().to_bytes();
        assert!(MacKeychain.get(&account).unwrap().is_none());
        MacKeychain.put(&account, &bytes).unwrap();
        let got = MacKeychain.get(&account);
        security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, &account).unwrap();
        assert_eq!(got.unwrap().unwrap(), bytes);
        assert!(MacKeychain.get(&account).unwrap().is_none());
    }
}
