//! Installs T3 Code and its provider tools as the guest owner. Provider
//! sign-in stays owner-managed.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use crate::ctx::{Ctx, is_executable};
use crate::error::{Result, fail};
use crate::fsutil::write_atomic;
use crate::process;
use crate::providers::{self, PROVIDERS};

const LIBATOMIC: &str = "/usr/lib/x86_64-linux-gnu/libatomic.so.1";
const SERVICE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The install job: system prerequisite, T3 itself, provider tools, service.
pub fn install(ctx: &Ctx, version: &str) -> Result<()> {
    let ctx = &ctx.with_deadline(Duration::from_secs(1200));
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return fail("T3 Code setup requires a Linux x86-64 guest.");
    }
    if !Path::new(LIBATOMIC).exists() {
        let mut command = ctx.tool("sudo");
        command.args(["-n", "apt-get", "install", "-y", "libatomic1"]);
        if !process::status(command, ctx.limit(Duration::from_secs(300))?)?
            .is_some_and(|status| status.success())
        {
            return fail(
                "Could not install libatomic1, which T3 Code needs. Check the VM's internet access and try again.",
            );
        }
    }
    let uid = nix::unistd::getuid();
    let mut ctx = ctx.with_path_first(&ctx.bin_dir());
    let runtime = format!("/run/user/{uid}");
    ctx.set_var(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={runtime}/bus"),
    );
    ctx.set_var("XDG_RUNTIME_DIR", runtime);
    setup(&ctx, version, &PROVIDERS)
}

fn installed(ctx: &Ctx, version: &str) -> bool {
    if !is_executable(&ctx.t3()) {
        return false;
    }
    let mut command = ctx.command(ctx.t3());
    command.arg("--version");
    let Ok(timeout) = ctx.limit(Duration::from_secs(30)) else {
        return false;
    };
    process::capture(command, timeout, 4096)
        .ok()
        .flatten()
        .is_some_and(|output| {
            output.success() && output.text().trim_end_matches('\n') == format!("t3 v{version}")
        })
}

fn run_installer(ctx: &Ctx, version: &str) -> Result<()> {
    let stage = tempfile::Builder::new().prefix("homecloud-t3-").tempdir()?;
    let installer = stage.path().join("install.sh");
    let mut curl = ctx.tool("curl");
    curl.args([
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
        "300",
        "-o",
    ])
    .arg(&installer)
    .arg("https://t3.codes/install.sh");
    if !process::status(curl, ctx.limit(Duration::from_secs(330))?)?
        .is_some_and(|status| status.success())
    {
        return fail(
            "Could not download the T3 Code installer. Check the VM's internet access and try again.",
        );
    }
    let mut shell = ctx.tool("sh");
    shell.arg(&installer).env("T3CODE_VERSION", version);
    if !process::status(shell, ctx.limit(Duration::from_secs(900))?)?
        .is_some_and(|status| status.success())
    {
        return fail("The T3 Code installer failed. Try again.");
    }
    Ok(())
}

/// SSH forwards reach loopback; no guest T3 port is exposed to other VMs or the LAN.
fn service_env(ctx: &Ctx) -> Result<()> {
    let path = ctx.home.join(".t3/service.env");
    match fs::read_to_string(&path) {
        Ok(existing) => {
            eprintln!("Existing service.env retained; it must contain T3CODE_HOST=127.0.0.1.");
            if !existing.lines().any(|line| line == "T3CODE_HOST=127.0.0.1") {
                return fail(
                    "~/.t3/service.env must contain T3CODE_HOST=127.0.0.1 so T3 Code listens only inside the VM. Fix it, then try again.",
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(b"T3CODE_HOST=127.0.0.1\nT3CODE_PORT=3773\n")?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn systemctl(ctx: &Ctx, args: &[&str]) -> Result<Option<process::Captured>> {
    let mut command = ctx.tool("systemctl");
    command.arg("--user").args(args);
    Ok(process::capture(
        command,
        ctx.limit(Duration::from_secs(60))?,
        65536,
    )?)
}

/// The PATH of the running T3 service, from its process environment.
fn running_path(ctx: &Ctx) -> Option<String> {
    let output = systemctl(
        ctx,
        &["show", "--property=MainPID", "--value", "t3code.service"],
    )
    .ok()??;
    let pid = output.text().trim().to_owned();
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let environ = fs::read(format!("/proc/{pid}/environ")).ok()?;
    environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"PATH="))
        .map(|path| String::from_utf8_lossy(path).into_owned())
}

/// Installing a unit can leave its old process running, so an explicit
/// repair restarts T3 when its process lacks the provider PATH, or when T3
/// itself was just installed. A process that already has the PATH finds new
/// tools itself, so adding a tool doesn't interrupt the owner's sessions.
fn needs_restart(ctx: &Ctx, installed_t3: bool) -> bool {
    let active = systemctl(ctx, &["is-active", "--quiet", "t3code.service"])
        .ok()
        .flatten()
        .is_some_and(|output| output.success());
    if !active {
        return false;
    }
    let local = format!(":{}:", ctx.bin_dir().display());
    match running_path(ctx) {
        Some(path) if format!(":{path}:").contains(&local) => installed_t3,
        _ => true,
    }
}

fn t3_service(ctx: &Ctx, action: &str) -> Result<()> {
    let mut command = ctx.command(ctx.t3());
    command.args(["service", action]);
    if !process::capture(command, ctx.limit(Duration::from_secs(120))?, 65536)?
        .is_some_and(|output| output.success())
    {
        return fail("Could not install the T3 Code service. Try again.");
    }
    Ok(())
}

pub fn setup(ctx: &Ctx, version: &str, providers: &[providers::Provider]) -> Result<()> {
    let ctx = &ctx.with_deadline(Duration::from_secs(1200));
    fs::create_dir_all(ctx.home.join(".t3"))?;
    fs::create_dir_all(ctx.bin_dir())?;
    let installed_t3 = !installed(ctx, version);
    if installed_t3 {
        run_installer(ctx, version)?;
    }
    service_env(ctx)?;
    // Provider tools are prerequisites of this T3 release. The native
    // distributions don't need Node or npm.
    providers::ensure(ctx, providers)?;
    // Keep provider discovery deterministic in the unattended user service.
    let mut path = OsString::from("PATH=");
    path.push(ctx.bin_dir());
    path.push(":");
    path.push(SERVICE_PATH);
    let dropin = format!("[Service]\nEnvironment=\"{}\"\n", path.to_string_lossy());
    write_atomic(
        &ctx.home
            .join(".config/systemd/user/t3code.service.d/homecloud.conf"),
        dropin.as_bytes(),
    )?;
    let restart = needs_restart(ctx, installed_t3);
    if !systemctl(ctx, &["daemon-reload"])?.is_some_and(|output| output.success()) {
        return fail("Could not reload the T3 Code service. Try again.");
    }
    t3_service(ctx, "install")?;
    if restart {
        t3_service(ctx, "restart")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;

    fn check_service_activation(active: bool, running_path: Option<&str>) {
        let fx = Fixture::new();
        let calls = fx.root.path().join("calls");
        let bin = fx.ctx.bin_dir();
        fs::create_dir_all(&bin).unwrap();
        let t3 = format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 't3 v0.1.2'; else echo \"$*\" >> {}; fi\n",
            calls.display()
        );
        fx.script(&bin.join("t3"), &t3);
        fx.tool("curl", "#!/bin/sh\nexit 99\n");
        for (name, version) in [
            ("codex", "0.165.0"),
            ("claude", "2.1.308"),
            ("gh", "2.103.3"),
        ] {
            fx.script(
                &bin.join(name),
                &format!("#!/bin/sh\necho '{name} {version}'\n"),
            );
        }
        let mut service = running_path.map(|path| {
            // Stand-in for the running T3 process; only its environment is read.
            std::process::Command::new("sleep")
                .arg("30")
                .env(
                    "PATH",
                    path.replace("HOME", &fx.ctx.home.display().to_string()),
                )
                .spawn()
                .unwrap()
        });
        let pid = service.as_ref().map_or(0, |child| child.id());
        let systemctl = format!(
            "#!/bin/sh\nif [ \"$2\" = is-active ]; then exit {}; fi\nif [ \"$2\" = show ]; then echo {pid}; exit 0; fi\necho \"$*\" >> {}\n",
            if active { 0 } else { 3 },
            calls.display()
        );
        fx.tool("systemctl", &systemctl);
        let owner = fx.ctx.home.join(".t3/userdata/owner-sentinel");
        fs::create_dir_all(owner.parent().unwrap()).unwrap();
        fs::write(&owner, "preserve owner state").unwrap();
        let envfile = fx.ctx.home.join(".t3/service.env");
        fs::write(&envfile, "T3CODE_HOST=127.0.0.1\nT3CODE_PORT=3773\n").unwrap();

        let result = setup(&fx.ctx, "0.1.2", &PROVIDERS);
        if let Some(child) = service.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        result.unwrap();
        assert_eq!(fs::read_to_string(&owner).unwrap(), "preserve owner state");
        assert_eq!(
            fs::read_to_string(&envfile).unwrap(),
            "T3CODE_HOST=127.0.0.1\nT3CODE_PORT=3773\n"
        );
        let dropin = fs::read_to_string(
            fx.ctx
                .home
                .join(".config/systemd/user/t3code.service.d/homecloud.conf"),
        )
        .unwrap();
        assert!(dropin.contains(&bin.display().to_string()));
        let events = fs::read_to_string(&calls).unwrap();
        let count = |event: &str| events.lines().filter(|line| *line == event).count();
        assert_eq!(count("--user daemon-reload"), 1);
        assert_eq!(count("service install"), 1);
        let restarts = if active && running_path.is_none() {
            1
        } else {
            0
        };
        assert_eq!(count("service restart"), restarts);
    }

    #[test]
    fn repair_restarts_existing_service_to_activate_provider_path() {
        check_service_activation(true, None);
    }

    #[test]
    fn initial_service_install_starts_once_and_preserves_owner_state() {
        check_service_activation(false, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn adding_a_tool_does_not_restart_a_service_that_already_has_the_tool_path() {
        check_service_activation(true, Some("HOME/.local/bin:/usr/bin:/bin"));
    }

    #[test]
    fn service_env_must_keep_t3_on_loopback() {
        let fx = Fixture::new();
        fs::create_dir_all(fx.ctx.home.join(".t3")).unwrap();
        service_env(&fx.ctx).unwrap();
        let path = fx.ctx.home.join(".t3/service.env");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "T3CODE_HOST=127.0.0.1\nT3CODE_PORT=3773\n"
        );
        fs::write(&path, "T3CODE_HOST=0.0.0.0\n").unwrap();
        assert!(
            service_env(&fx.ctx)
                .unwrap_err()
                .user_message()
                .contains("127.0.0.1")
        );
    }
}
