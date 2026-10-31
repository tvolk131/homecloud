use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::Path;

use nix::fcntl::{Flock, FlockArg};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

pub fn private_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

/// Missing, unreadable or malformed state reads as absent.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Durable replace: a reader sees the old or the new file, never a mix.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Internal("state path has no parent".into()))?;
    private_dir(parent)?;
    let mut file = tempfile::Builder::new()
        .prefix(".job-")
        .tempfile_in(parent)?;
    file.write_all(data)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_atomic(path, &serde_json::to_vec(value)?)
}

/// An exclusive lock, or `None` while another process holds it.
pub fn try_lock(path: &Path) -> io::Result<Option<Flock<File>>> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(Some(lock)),
        Err((_, nix::errno::Errno::EWOULDBLOCK)) => Ok(None),
        Err((_, error)) => Err(error.into()),
    }
}

pub fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec())
}

/// Identifies an installed binary: resolved path, modification time and size.
pub fn stamp_of(path: &Path) -> Option<(String, i64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let resolved = fs::canonicalize(path).ok()?;
    Some((
        resolved.to_string_lossy().into_owned(),
        mtime_ns(&meta),
        meta.size(),
    ))
}
