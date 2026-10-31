//! Disposable guest homes with fake provider tools.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tempfile::TempDir;

use crate::ctx::{Ctx, LOGIN_TIMEOUT};

pub struct Fixture {
    pub root: TempDir,
    /// Fake tools, searched after `~/.local/bin`. Nothing else is on PATH,
    /// so a real CLI on the test machine is never used by accident.
    pub bin: PathBuf,
    pub ctx: Ctx,
}

fn system_tool(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| crate::ctx::is_executable(candidate))
        .unwrap_or_else(|| panic!("tests need {name} on PATH"))
}

impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let bin = root.path().join("tools");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let path = std::env::join_paths([home.join(".local/bin"), bin.clone()]).unwrap();
        let env = BTreeMap::from([("HOME".into(), home.clone().into()), ("PATH".into(), path)]);
        let ctx = Ctx {
            home,
            env,
            boot_id_path: root.path().join("boot"),
            login_timeout: LOGIN_TIMEOUT,
        };
        let fixture = Self { root, bin, ctx };
        fixture.write("boot", "fixture-boot");
        fixture
    }

    /// Appends a directory to the end of PATH.
    pub fn with_path_after(mut self, dir: &Path) -> Self {
        let current = self.ctx.var("PATH").unwrap_or_default().to_owned();
        let mut paths: Vec<PathBuf> = std::env::split_paths(&current).collect();
        paths.push(dir.to_path_buf());
        self.ctx
            .set_var("PATH", std::env::join_paths(paths).unwrap());
        self
    }

    pub fn write(&self, name: &str, content: &str) {
        fs::write(self.root.path().join(name), content).unwrap();
    }

    pub fn dir(&self, name: &str) -> PathBuf {
        let path = self.root.path().join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    pub fn script(&self, path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Replace rather than rewrite, so a running copy is never modified.
        let temporary = path.with_extension("fixture-new");
        fs::write(&temporary, content).unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&temporary, path).unwrap();
    }

    pub fn tool(&self, name: &str, content: &str) {
        self.script(&self.bin.join(name), content);
    }

    /// A fake tool written in Python, run by the test machine's interpreter.
    pub fn python_tool(&self, name: &str, body: &str) {
        let shebang = format!("#!{}\n", system_tool("python3").display());
        self.tool(name, &(shebang + body));
    }

    /// Makes a real tool from the test machine available.
    pub fn link_system_tool(&self, name: &str) {
        std::os::unix::fs::symlink(system_tool(name), self.bin.join(name)).unwrap();
    }
}

pub fn read_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
