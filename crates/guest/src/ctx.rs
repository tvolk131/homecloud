use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::{Error, Result};

/// How long Homecloud waits for an owner to finish a provider sign-in. This is
/// a local limit, not a promise about the provider's own code lifetime.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);

/// The guest owner's environment. Every child process gets exactly this
/// environment, so tests can supply fake tools without touching global state.
#[derive(Clone, Debug)]
pub struct Ctx {
    pub home: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub boot_id_path: PathBuf,
    pub login_timeout: Duration,
}

impl Ctx {
    pub fn from_process() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or_else(|| Error::Internal("HOME is not an absolute path".into()))?;
        Ok(Self {
            home,
            env: std::env::vars_os().collect(),
            boot_id_path: PathBuf::from("/proc/sys/kernel/random/boot_id"),
            login_timeout: LOGIN_TIMEOUT,
        })
    }

    /// Private per-application job state.
    pub fn base(&self) -> PathBuf {
        self.home.join(".local/state/homecloud/apps")
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.home.join(".local/bin")
    }

    pub fn t3(&self) -> PathBuf {
        self.bin_dir().join("t3")
    }

    pub fn boot_id(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.boot_id_path)?.trim().to_owned())
    }

    pub fn var(&self, name: &str) -> Option<&OsStr> {
        self.env.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    pub fn set_var(&mut self, name: &str, value: impl Into<OsString>) {
        self.env.insert(name.into(), value.into());
    }

    /// The same context with `dir` searched first.
    pub fn with_path_first(&self, dir: &Path) -> Self {
        let mut paths = vec![dir.to_path_buf()];
        if let Some(current) = self.var("PATH") {
            paths.extend(std::env::split_paths(current).filter(|p| p != dir));
        }
        let mut ctx = self.clone();
        let joined = std::env::join_paths(paths).unwrap_or_else(|_| dir.as_os_str().to_owned());
        ctx.set_var("PATH", joined);
        ctx
    }

    /// Like `shutil.which`, over this context's PATH.
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        let path = self.var("PATH")?;
        std::env::split_paths(path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    }

    /// A command with this context's environment, the home directory as its
    /// working directory and no stdin.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .envs(&self.env)
            .current_dir(&self.home)
            .stdin(Stdio::null());
        command
    }

    /// A command for a named tool, resolved on this context's PATH.
    pub fn tool(&self, name: &str) -> Command {
        match self.which(name) {
            Some(path) => self.command(path),
            None => self.command(name),
        }
    }
}

pub fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
