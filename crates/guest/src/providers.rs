//! T3's native provider tools, installed as the guest owner without login.
//!
//! Pins satisfy T3 0.1.2's bundled compatibility policy. Compatible existing
//! installations are kept. Downloads are checked against pinned SHA-256
//! digests before they are executed or extracted.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, BufReader};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use flate2::read::GzDecoder;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tar::EntryType;

use crate::ctx::{Ctx, is_executable};
use crate::error::{Error, Result, fail};
use crate::metadata::AccountInfo;
use crate::process;
use crate::text::{find_version, parse_version};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Codex,
    Claude,
    GitHub,
}

/// A provider tool: how T3 finds it, the oldest release T3 supports, and
/// the release Homecloud installs when none is usable.
#[derive(Clone, Debug)]
pub struct Provider {
    pub binary: &'static str,
    /// Component name shown to the owner and used by setup actions.
    pub name: &'static str,
    pub minimum: [u64; 3],
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// An outdated copy outside `~/.local/bin` (for example Ubuntu's own
    /// package) is shadowed rather than refused.
    pub shadow_system: bool,
    pub kind: Kind,
}

pub const PROVIDERS: [Provider; 3] = [
    Provider {
        binary: "codex",
        name: "Codex CLI",
        minimum: [0, 159, 0],
        version: "0.165.0",
        url: "https://github.com/openai/codex/releases/download/rust-v0.165.0/codex-package-x86_64-unknown-linux-musl.tar.gz",
        sha256: "e97c4463d7f146692a955d7b0e1c8cf48f97cd4d616cd45c5dcab2e8d9743316",
        shadow_system: false,
        kind: Kind::Codex,
    },
    Provider {
        binary: "claude",
        name: "Claude Code",
        minimum: [2, 1, 280],
        version: "2.1.308",
        url: "https://downloads.claude.ai/claude-code-releases/2.1.308/linux-x64/claude",
        sha256: "356733f185fa988e6bf3aa9a555269fdd167982a2e2a80d3c8a78037bb92ad58",
        shadow_system: false,
        kind: Kind::Claude,
    },
    // 2.40 added per-account logout and --insecure-storage, both used by sign-in and sign-out.
    Provider {
        binary: "gh",
        name: "GitHub CLI",
        minimum: [2, 40, 0],
        version: "2.103.3",
        url: "https://github.com/cli/cli/releases/download/v2.103.3/gh_2.103.3_linux_amd64.tar.gz",
        sha256: "4c806978ffce1cec70853302a56d70ac6da4f19bae7adca4e1a8ab00633dd91d",
        shadow_system: true,
        kind: Kind::GitHub,
    },
];

impl Provider {
    pub fn by_name(name: &str) -> Option<&'static Provider> {
        PROVIDERS.iter().find(|provider| provider.name == name)
    }

    pub fn account_info(&self, ctx: &Ctx) -> AccountInfo {
        match self.kind {
            Kind::Codex => crate::codex::account_info(ctx),
            Kind::Claude => crate::claude::account_info(ctx),
            Kind::GitHub => crate::github::account_info(ctx),
        }
    }
}

/// `binary --version`, if it runs and reports a release.
pub fn version(ctx: &Ctx, binary: &Path) -> Option<[u64; 3]> {
    let mut command = ctx.command(binary);
    command.arg("--version");
    let output =
        process::capture(command, ctx.limit(Duration::from_secs(30)).ok()?, 65536).ok()??;
    if !output.success() {
        return None;
    }
    parse_version(&find_version(&output.text(), false)?)
}

/// Installs every provider that is missing or too old.
pub fn ensure(ctx: &Ctx, providers: &[Provider]) -> Result<()> {
    let bin_dir = ctx.bin_dir();
    fs::create_dir_all(&bin_dir)?;
    for provider in providers {
        if let Err(error) = ensure_one(ctx, provider, &bin_dir) {
            if provider.binary != "gh" {
                return Err(error);
            }
            eprintln!(
                "gh: optional installation failed; existing tools retained. Repair GitHub CLI separately."
            );
        }
        ctx.limit(Duration::from_secs(1))?;
    }
    Ok(())
}

fn ensure_one(ctx: &Ctx, provider: &Provider, bin_dir: &Path) -> Result<()> {
    if let Some(current) = ctx.which(provider.binary) {
        let installed = version(ctx, &current);
        ctx.limit(Duration::from_secs(1))?;
        if installed.is_some_and(|found| found >= provider.minimum) {
            eprintln!("{}: compatible installation retained", provider.binary);
            return Ok(());
        }
        // ~/.local/bin precedes system directories for T3 and login shells.
        if !provider.shadow_system || current.parent() == Some(bin_dir) {
            return fail(format!(
                "{}: the existing installation is unsupported or broken. Update or remove it, then try again.",
                provider.name
            ));
        }
    }
    eprintln!("Installing {} {}", provider.binary, provider.version);
    let stage = tempfile::Builder::new()
        .prefix("homecloud-provider-")
        .tempdir()?;
    let artifact = stage.path().join("download");
    download(ctx, provider, &artifact)?;
    let root = ctx
        .home
        .join(".local/share/homecloud/providers")
        .join(provider.binary);
    match provider.kind {
        Kind::Codex => {
            fs::create_dir_all(&root)?;
            let extraction = tempfile::Builder::new()
                .prefix(".stage-")
                .tempdir_in(&root)?;
            unpack_codex(&artifact, extraction.path(), provider.version)?;
            ctx.limit(Duration::from_secs(1))?;
            let target = replace_dir(extraction.path(), &root.join(provider.version))?;
            link(&target.join("bin/codex"), &bin_dir.join("codex"))?;
            link(
                &target.join("bin/codex-code-mode-host"),
                &bin_dir.join("codex-code-mode-host"),
            )?;
        }
        Kind::GitHub => {
            fs::create_dir_all(&root)?;
            let extraction = tempfile::Builder::new()
                .prefix(".stage-")
                .tempdir_in(&root)?;
            let package = unpack_gh(&artifact, extraction.path(), provider.version)?;
            ctx.limit(Duration::from_secs(1))?;
            let target = replace_dir(&package, &root.join(provider.version))?;
            link(&target.join("bin/gh"), &bin_dir.join("gh"))?;
        }
        Kind::Claude => {
            fs::set_permissions(&artifact, fs::Permissions::from_mode(0o700))?;
            // The vendor's own installer creates its standard native layout,
            // keeping Claude's and T3's update tools working.
            let mut command = ctx.command(&artifact);
            command
                .args(["install", provider.version])
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if !process::status(command, ctx.limit(Duration::from_secs(300))?)?
                .is_some_and(|status| status.success())
            {
                return fail("Claude Code installation failed. Try again.");
            }
        }
    }
    if version(ctx, &bin_dir.join(provider.binary)) != parse_version(provider.version) {
        return fail(format!(
            "{}: the installed version does not match the requested release.",
            provider.name
        ));
    }
    eprintln!("{}: installed and verified", provider.binary);
    Ok(())
}

fn download(ctx: &Ctx, provider: &Provider, path: &Path) -> Result<()> {
    let mut command = ctx.tool("curl");
    command
        .args([
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "-fsSL",
        ])
        .args([
            "--retry",
            "2",
            "--connect-timeout",
            "30",
            "--max-time",
            "900",
        ])
        .args(["-A", "homecloud-guest-installer", "-o"])
        .arg(path)
        .arg(provider.url);
    let downloaded = process::status(command, ctx.limit(Duration::from_secs(960))?)?
        .is_some_and(|status| status.success());
    if !downloaded {
        ctx.limit(Duration::from_secs(1))?;
        return fail(format!(
            "Could not download {}. Check the VM's internet access and try again.",
            provider.name
        ));
    }
    if sha256_file(path)? != provider.sha256 {
        let _ = fs::remove_file(path);
        return fail(format!(
            "{} download did not match its pinned checksum. Try again later.",
            provider.name
        ));
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

fn replace_dir(source: &Path, target: &Path) -> Result<PathBuf> {
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::rename(source, target)?;
    Ok(target.to_path_buf())
}

/// Points `destination` at `target` in one rename.
fn link(target: &Path, destination: &Path) -> Result<()> {
    let mut name = destination.file_name().unwrap_or_default().to_owned();
    name.push(".homecloud-new");
    let temporary = destination.with_file_name(name);
    match fs::remove_file(&temporary) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    std::os::unix::fs::symlink(target, &temporary)?;
    fs::rename(&temporary, destination)?;
    Ok(())
}

const UNSAFE: &str = "A provider package contains an unsafe or unsupported entry. Try again later.";

fn unsafe_entry<T>() -> Result<T> {
    fail(UNSAFE)
}

/// A relative path with only normal components; `None` for the archive root.
fn entry_path(raw: &Path) -> Result<Option<PathBuf>> {
    let mut path = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return unsafe_entry(),
        }
    }
    Ok((!path.as_os_str().is_empty()).then_some(path))
}

/// A link target must be relative, may only climb with leading `..`, and
/// must stay inside the archive. Because no entry is ever written through a
/// symlink, resolving such a target can't leave the destination either.
fn safe_link(entry: &Path, target: &Path) -> bool {
    let mut climbs = 0;
    let mut descended = false;
    for component in target.components() {
        match component {
            Component::ParentDir if !descended => climbs += 1,
            Component::Normal(_) => descended = true,
            Component::CurDir => {}
            _ => return false,
        }
    }
    let depth = entry.components().count() - 1;
    !target.as_os_str().is_empty() && climbs <= depth
}

/// Creates missing parents as real directories; refuses to pass through
/// anything else, including symlinks from earlier entries.
fn parents(destination: &Path, path: &Path) -> Result<()> {
    let mut current = destination.to_path_buf();
    let parts: Vec<_> = path.components().collect();
    for part in &parts[..parts.len() - 1] {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return unsafe_entry(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                DirBuilder::new().mode(0o755).create(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Extracts regular files, directories and contained symlinks only. Modes
/// lose setuid, setgid and group or other write access.
pub fn extract(archive: &Path, destination: &Path) -> Result<()> {
    let file = File::open(archive)?;
    let mut tar = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        let Some(path) = entry_path(&entry.path()?)? else {
            continue;
        };
        parents(destination, &path)?;
        let target = destination.join(&path);
        // Nothing replaces, or is written through, an earlier entry.
        let existing = fs::symlink_metadata(&target).ok();
        match kind {
            EntryType::Directory => match existing {
                Some(meta) if meta.is_dir() => {}
                Some(_) => return unsafe_entry(),
                None => DirBuilder::new().mode(0o755).create(&target)?,
            },
            _ if existing.is_some() => return unsafe_entry(),
            EntryType::Regular | EntryType::Continuous => {
                let mode = (entry.header().mode()? & 0o755) | 0o600;
                // create_new also refuses anything created since the check.
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(mode)
                    .open(&target)?;
                io::copy(&mut entry, &mut file)?;
            }
            EntryType::Symlink => {
                let link = entry
                    .link_name()?
                    .ok_or_else(|| Error::Authored(UNSAFE.into()))?;
                if !safe_link(&path, &link) {
                    return unsafe_entry();
                }
                std::os::unix::fs::symlink(&link, &target)?;
            }
            _ => return unsafe_entry(),
        }
    }
    Ok(())
}

pub fn unpack_codex(archive: &Path, destination: &Path, expected: &str) -> Result<()> {
    extract(archive, destination)?;
    let manifest: Option<Value> = fs::read(destination.join("codex-package.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok());
    let field = |key| {
        manifest
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(Value::as_str)
    };
    if field("version") != Some(expected)
        || field("target") != Some("x86_64-unknown-linux-musl")
        || field("entrypoint") != Some("bin/codex")
    {
        return fail("Unexpected Codex package manifest. Try again later.");
    }
    for relative in ["bin/codex", "bin/codex-code-mode-host", "codex-path/rg"] {
        if !is_executable(&destination.join(relative)) {
            return fail("Incomplete Codex native package. Try again later.");
        }
    }
    Ok(())
}

pub fn unpack_gh(archive: &Path, destination: &Path, expected: &str) -> Result<PathBuf> {
    extract(archive, destination)?;
    let root = destination.join(format!("gh_{expected}_linux_amd64"));
    let binary = root.join("bin/gh");
    let regular = fs::symlink_metadata(&binary).is_ok_and(|meta| meta.file_type().is_file());
    if !regular || !is_executable(&binary) {
        return fail("Incomplete GitHub CLI package. Try again later.");
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Read;

    fn read_limited(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        File::open(path)?.take(limit).read_to_end(&mut out)?;
        Ok(out)
    }

    enum Item<'a> {
        File(&'a str, &'a [u8], u32),
        Link(&'a str, &'a str),
        Hard(&'a str, &'a str),
        Dir(&'a str),
    }

    fn archive(path: &Path, items: &[Item<'_>]) {
        let mut builder = tar::Builder::new(GzEncoder::new(
            File::create(path).unwrap(),
            Compression::fast(),
        ));
        for item in items {
            let mut header = tar::Header::new_gnu();
            match item {
                Item::File(name, data, mode) => {
                    header.set_size(data.len() as u64);
                    header.set_mode(*mode);
                    header.set_entry_type(EntryType::Regular);
                    // Write names raw so tests can include unsafe paths.
                    header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                    header.set_cksum();
                    builder.append(&header, *data).unwrap();
                }
                Item::Link(name, target) | Item::Hard(name, target) => {
                    header.set_size(0);
                    header.set_mode(0o777);
                    let kind = if matches!(item, Item::Link(..)) {
                        EntryType::Symlink
                    } else {
                        EntryType::Link
                    };
                    header.set_entry_type(kind);
                    header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                    header.as_old_mut().linkname[..target.len()].copy_from_slice(target.as_bytes());
                    header.set_cksum();
                    builder.append(&header, io::empty()).unwrap();
                }
                Item::Dir(name) => {
                    header.set_size(0);
                    header.set_mode(0o755);
                    header.set_entry_type(EntryType::Directory);
                    header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                    header.set_cksum();
                    builder.append(&header, io::empty()).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    fn codex_items(manifest: &[u8]) -> Vec<Item<'_>> {
        vec![
            Item::File("codex-package.json", manifest, 0o644),
            Item::File("bin/codex", b"cli", 0o755),
            Item::File("bin/codex-code-mode-host", b"host", 0o4775),
            Item::File("codex-path/rg", b"rg", 0o755),
            Item::Link("codex-path/codex", "../bin/codex"),
        ]
    }

    const MANIFEST: &[u8] =
        br#"{"version":"0.165.0","target":"x86_64-unknown-linux-musl","entrypoint":"bin/codex"}"#;

    #[test]
    fn codex_package_checks_manifest_and_required_code_mode_sidecars() {
        let fx = Fixture::new();
        let tgz = fx.root.path().join("package.tgz");
        archive(&tgz, &codex_items(MANIFEST));
        let destination = fx.dir("unpacked");
        unpack_codex(&tgz, &destination, "0.165.0").unwrap();
        let host = fs::metadata(destination.join("bin/codex-code-mode-host")).unwrap();
        assert_eq!(
            host.permissions().mode() & 0o7022,
            0,
            "setuid and group write are removed"
        );
        assert_eq!(
            read_limited(&destination.join("codex-path/codex"), 16).unwrap(),
            b"cli"
        );
        let error = unpack_codex(&tgz, &fx.dir("again"), "0.100.0").unwrap_err();
        assert!(error.user_message().contains("manifest"));
        let mut partial = codex_items(MANIFEST);
        partial.remove(3);
        archive(&tgz, &partial);
        let error = unpack_codex(&tgz, &fx.dir("partial"), "0.165.0").unwrap_err();
        assert!(error.user_message().contains("Incomplete"));
    }

    #[test]
    fn github_cli_package_requires_the_pinned_layout() {
        let fx = Fixture::new();
        let tgz = fx.root.path().join("gh.tgz");
        archive(
            &tgz,
            &[
                Item::File("gh_2.103.3_linux_amd64/bin/gh", b"cli", 0o755),
                Item::File("gh_2.103.3_linux_amd64/LICENSE", b"mit", 0o644),
            ],
        );
        let root = unpack_gh(&tgz, &fx.dir("unpacked"), "2.103.3").unwrap();
        assert_eq!(root.file_name().unwrap(), "gh_2.103.3_linux_amd64");
        let error = unpack_gh(&tgz, &fx.dir("other"), "2.100.0").unwrap_err();
        assert!(error.user_message().contains("Incomplete"));
        archive(
            &tgz,
            &[
                Item::Dir("gh_2.103.3_linux_amd64/bin"),
                Item::Link("gh_2.103.3_linux_amd64/bin/gh", "../LICENSE"),
            ],
        );
        assert!(unpack_gh(&tgz, &fx.dir("linked"), "2.103.3").is_err());
    }

    #[test]
    fn packages_cannot_escape_the_staging_directory() {
        let cases: Vec<Vec<Item<'_>>> = vec![
            vec![Item::File("../escaped", b"x", 0o644)],
            vec![Item::File("/escaped", b"x", 0o644)],
            vec![Item::Link("link", "/etc")],
            vec![Item::Link("link", "../outside")],
            vec![Item::Link("dir/link", "../../outside")],
            vec![Item::Link("dir/link", "a/../../..")],
            // An entry may not be written through a symlink from an earlier one.
            vec![Item::Link("up", "."), Item::File("up/escaped", b"x", 0o644)],
            vec![
                Item::File("target", b"x", 0o644),
                Item::Link("alias", "target"),
                Item::File("alias", b"y", 0o644),
            ],
            vec![Item::File("data", b"x", 0o644), Item::Hard("copy", "data")],
        ];
        for (index, items) in cases.iter().enumerate() {
            let fx = Fixture::new();
            let tgz = fx.root.path().join("package.tgz");
            archive(&tgz, items);
            let destination = fx.dir("stage/unpacked");
            let error = extract(&tgz, &destination).unwrap_err();
            assert_eq!(error.user_message(), UNSAFE, "case {index}");
            assert!(
                !fx.root.path().join("stage/escaped").exists()
                    && !fx.root.path().join("escaped").exists()
            );
            if let Ok(kept) = fs::read(destination.join("target")) {
                assert_eq!(kept, b"x", "case {index}");
            }
        }
        let fx = Fixture::new();
        let tgz = fx.root.path().join("package.tgz");
        archive(
            &tgz,
            &[
                Item::Dir("dir"),
                Item::Link("dir/link", "../dir"),
                Item::Link("sibling", "./dir/link"),
            ],
        );
        extract(&tgz, &fx.dir("ok")).unwrap();
    }

    fn version_tool(dir: &Path, name: &str, version: &str) {
        let path = dir.join(name);
        fs::create_dir_all(dir).unwrap();
        fs::write(
            &path,
            format!("#!/bin/sh\necho \"{name} version {version}\"\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Download attempts are logged and fail before any network access.
    fn failing_curl(fx: &Fixture) {
        let log = fx.root.path().join("downloads");
        fx.tool(
            "curl",
            &format!(
                "#!/bin/sh\nfor last; do :; done\necho \"$last\" >> {}\nexit 22\n",
                log.display()
            ),
        );
    }

    fn downloads(fx: &Fixture) -> Vec<String> {
        fs::read_to_string(fx.root.path().join("downloads"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn existing_compatible_installations_are_retained_without_login_or_download() {
        let fx = Fixture::new();
        failing_curl(&fx);
        let bin = fx.bin.clone();
        version_tool(&bin, "codex", "0.165.0");
        version_tool(&bin, "claude", "2.1.308");
        version_tool(&bin, "gh", "2.45.0");
        ensure(&fx.ctx, &PROVIDERS).unwrap();
        assert!(downloads(&fx).is_empty());
    }

    #[test]
    fn outdated_system_github_cli_is_shadowed_but_a_local_one_is_not_replaced() {
        let gh = &PROVIDERS[2..];
        let fx = Fixture::new();
        failing_curl(&fx);
        let system = fx.root.path().join("system");
        version_tool(&system, "gh", "2.20.0");
        let fx = fx.with_path_after(&system);
        ensure(&fx.ctx, gh).unwrap();
        assert_eq!(downloads(&fx), [PROVIDERS[2].url]);

        let fx = Fixture::new();
        failing_curl(&fx);
        version_tool(&fx.ctx.bin_dir(), "gh", "2.20.0");
        ensure(&fx.ctx, gh).unwrap();
        assert!(downloads(&fx).is_empty());
    }

    #[test]
    fn installation_budget_limits_download_and_cleans_its_stage() {
        let fx = Fixture::new();
        fx.link_system_tool("sleep");
        let marker = fx.root.path().join("stage-path");
        fx.tool("curl", &format!("#!/bin/sh\nprevious=''\nfor argument do\nif [ \"$previous\" = -o ]; then printf '%s' \"$argument\" > '{}'; fi\nprevious=\"$argument\"\ndone\nsleep 10\n", marker.display()));
        let ctx = fx.ctx.with_deadline(Duration::from_secs(1));
        let started = std::time::Instant::now();
        let error = ensure(&ctx, &PROVIDERS[..1]).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        match fs::read_to_string(marker) {
            Ok(path) => assert!(!PathBuf::from(path).parent().unwrap().exists()),
            Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                assert!(error.user_message().contains("timed out"))
            }
            Err(error) => panic!("{error}"),
        }
        assert!(
            ctx.with_deadline(Duration::from_secs(60))
                .limit(Duration::from_secs(1))
                .is_err()
        );
    }

    #[test]
    fn broken_existing_installation_is_not_silently_replaced() {
        let fx = Fixture::new();
        failing_curl(&fx);
        let bin = fx.bin.clone();
        version_tool(&bin, "codex", "0.100.0");
        let error = ensure(&fx.ctx, &PROVIDERS).unwrap_err();
        assert!(
            error
                .user_message()
                .contains("Codex CLI: the existing installation is unsupported")
        );
        assert!(downloads(&fx).is_empty());
    }

    #[test]
    fn download_rejects_a_changed_artifact_and_removes_it() {
        let fx = Fixture::new();
        fx.tool(
            "curl",
            "#!/bin/sh\nwhile [ \"$1\" != -o ]; do shift; done\nprintf 'wrong package' > \"$2\"\n",
        );
        let output = fx.root.path().join("artifact");
        let provider = Provider {
            sha256: "0000",
            ..PROVIDERS[2].clone()
        };
        let error = download(&fx.ctx, &provider, &output).unwrap_err();
        assert!(error.user_message().contains("checksum"));
        assert!(!output.exists());
    }

    #[test]
    fn verified_github_cli_download_is_linked_into_the_owner_path() {
        let fx = Fixture::new();
        let tgz = fx.root.path().join("gh.tgz");
        let script = b"#!/bin/sh\necho 'gh version 2.103.3 (2026-01-01)'\n";
        archive(
            &tgz,
            &[Item::File("gh_2.103.3_linux_amd64/bin/gh", script, 0o755)],
        );
        fx.tool(
            "curl",
            &format!(
                "#!/bin/sh\nwhile [ \"$1\" != -o ]; do shift; done\n/bin/cp {} \"$2\"\n",
                tgz.display()
            ),
        );
        let provider = Provider {
            sha256: Box::leak(sha256_file(&tgz).unwrap().into_boxed_str()),
            ..PROVIDERS[2].clone()
        };
        ensure(&fx.ctx, std::slice::from_ref(&provider)).unwrap();
        let linked = fs::read_link(fx.ctx.bin_dir().join("gh")).unwrap();
        assert!(linked.ends_with("providers/gh/2.103.3/bin/gh"));
        assert_eq!(
            version(&fx.ctx, &fx.ctx.bin_dir().join("gh")),
            Some([2, 103, 3])
        );
        // A rerun keeps the verified installation.
        fs::remove_file(fx.bin.join("curl")).unwrap();
        ensure(&fx.ctx, std::slice::from_ref(&provider)).unwrap();
    }
}
