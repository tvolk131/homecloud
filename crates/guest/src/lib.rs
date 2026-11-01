//! The fixed managed-application runner that Homecloud places in guest VMs.
//!
//! The host pushes this one static binary and runs it as the guest owner to
//! inspect T3 Code, install it with its provider tools, and supervise owner
//! sign-ins. Provider credentials stay with each provider's own CLI.

pub mod claude;
pub mod codex;
pub mod ctx;
pub mod error;
pub mod fsutil;
pub mod github;
pub mod install;
pub mod metadata;
pub mod process;
pub mod providers;
pub mod runner;
pub mod state;
pub mod t3;
#[cfg(test)]
mod testing;
pub mod text;

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or_default()
}

pub fn now() -> u64 {
    now_f64() as u64
}
