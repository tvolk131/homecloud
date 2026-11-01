use std::io::{self, Read};
use std::process::ExitCode;

use homecloud_core::{ApplicationKind, ApplicationOperationKind, ApplicationSetupAction};
use homecloud_guest::ctx::Ctx;
use homecloud_guest::error::{Error, Result};
use homecloud_guest::runner;
use serde_json::Value;
use uuid::Uuid;

const USAGE: &str = "\
Homecloud guest application runner. Runs inside a VM as its owner.

Usage:
  homecloud-guest inspect APP
  homecloud-guest job APP OPERATION KIND VERSION UNIT SETUP_ACTION
  homecloud-guest submit APP OPERATION    (reads {\"code\": ...} on stdin)
  homecloud-guest --version
";

fn usage<T>() -> Result<T> {
    eprint!("{USAGE}");
    Err(Error::Internal("invalid arguments".into()))
}

fn uuid(text: &str) -> Result<Uuid> {
    Ok(Uuid::parse_str(text)?)
}

fn valid_job(version: &str, unit: &str, app: Uuid, operation: Uuid) -> bool {
    version == ApplicationKind::T3.version()
        && unit
            == format!(
                "homecloud-app-{}-{}.service",
                app.simple(),
                operation.simple()
            )
}

fn run(args: &[String]) -> Result<()> {
    let ctx = Ctx::from_process()?;
    match args {
        [mode, app] if mode == "inspect" => {
            let report = runner::inspect(&ctx, uuid(app)?)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        [mode, app, operation] if mode == "submit" => {
            let mut input = Vec::new();
            io::stdin().take(16 * 1024).read_to_end(&mut input)?;
            let value: Value = serde_json::from_slice(&input)?;
            runner::submit(
                &ctx,
                uuid(app)?,
                uuid(operation)?,
                value.get("code").and_then(Value::as_str),
            )
        }
        [mode, app, operation, kind, version, unit, action] if mode == "job" => {
            let (app, operation) = (uuid(app)?, uuid(operation)?);
            let kind: ApplicationOperationKind =
                serde_json::from_value(Value::String(kind.clone()))?;
            let action: ApplicationSetupAction =
                serde_json::from_value(Value::String(action.clone()))?;
            if !valid_job(version, unit, app, operation) {
                return usage();
            }
            runner::job(&ctx, app, operation, kind, version, unit, action)
        }
        _ => usage(),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("--version") => {
            println!("homecloud-guest {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    if nix::unistd::getuid().is_root() {
        eprintln!("Run guest applications as the VM owner, not root.");
        return ExitCode::from(2);
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_version_and_unit_are_bound_to_the_operation() {
        let app = Uuid::from_u128(1);
        let operation = Uuid::from_u128(2);
        let unit = format!(
            "homecloud-app-{}-{}.service",
            app.simple(),
            operation.simple()
        );
        assert!(valid_job(
            ApplicationKind::T3.version(),
            &unit,
            app,
            operation
        ));
        assert!(!valid_job("unsupported", &unit, app, operation));
        assert!(!valid_job(
            ApplicationKind::T3.version(),
            "foreign.service",
            app,
            operation
        ));
        assert!(!valid_job(
            ApplicationKind::T3.version(),
            &unit,
            app,
            Uuid::from_u128(3)
        ));
    }
}
