//! Bounded child processes. Every child runs in its own session so a timeout
//! or cancellation can stop everything it started.

use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

#[derive(Debug)]
pub struct Captured {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
}

impl Captured {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

pub fn new_session(command: &mut Command) {
    // SAFETY: setsid is async-signal-safe and touches no parent state.
    unsafe {
        command.pre_exec(|| nix::unistd::setsid().map(drop).map_err(io::Error::from));
    }
}

/// Runs a command with stdout captured (at most `limit` bytes kept) and
/// stderr discarded. `Ok(None)` means it was killed at the deadline.
pub fn capture(command: Command, timeout: Duration, limit: usize) -> io::Result<Option<Captured>> {
    capture_output(command, timeout, limit, false)
}

pub fn capture_combined(
    command: Command,
    timeout: Duration,
    limit: usize,
) -> io::Result<Option<Captured>> {
    capture_output(command, timeout, limit, true)
}

fn capture_output(
    mut command: Command,
    timeout: Duration,
    limit: usize,
    combined: bool,
) -> io::Result<Option<Captured>> {
    let (mut stdout, writer) = io::pipe()?;
    command.stdout(writer.try_clone()?);
    if combined {
        command.stderr(writer);
    } else {
        command.stderr(Stdio::null());
        drop(writer);
    }
    new_session(&mut command);
    let deadline = Instant::now() + timeout;
    let mut child = Guard::new(command.spawn()?, Duration::ZERO);
    drop(command);
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        if !readable(stdout.as_fd(), deadline)? {
            return Ok(None);
        }
        let count = match stdout.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if count == 0 {
            break;
        }
        let room = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(room)]);
    }
    let Some(status) = child.wait_until(deadline)? else {
        return Ok(None);
    };
    Ok(Some(Captured {
        code: status.code(),
        stdout: output,
    }))
}

pub fn terminal(size: Option<&nix::pty::Winsize>) -> io::Result<nix::pty::OpenptyResult> {
    let terminal = nix::pty::openpty(size, None).map_err(io::Error::from)?;
    for descriptor in [&terminal.master, &terminal.slave] {
        nix::fcntl::fcntl(
            descriptor,
            nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
        )
        .map_err(io::Error::from)?;
    }
    Ok(terminal)
}

/// Runs a command with its own stdio. `Ok(None)` means it was killed at the deadline.
pub fn status(mut command: Command, timeout: Duration) -> io::Result<Option<ExitStatus>> {
    new_session(&mut command);
    let deadline = Instant::now() + timeout;
    Guard::new(command.spawn()?, Duration::ZERO).wait_until(deadline)
}

/// Waits until `fd` is readable or hung up. False once the deadline passes.
pub fn readable(fd: BorrowedFd<'_>, deadline: Instant) -> io::Result<bool> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        if ready(&[fd], remaining)?.into_iter().any(|ready| ready) {
            return Ok(true);
        }
    }
}

/// Polls several descriptors once; each entry says whether that one is ready.
pub fn ready(fds: &[BorrowedFd<'_>], wait: Duration) -> io::Result<Vec<bool>> {
    let mut polled: Vec<PollFd<'_>> = fds
        .iter()
        .map(|fd| PollFd::new(*fd, PollFlags::POLLIN))
        .collect();
    let millis = u16::try_from(wait.as_millis().clamp(1, 60_000)).unwrap_or(u16::MAX);
    match poll(&mut polled, PollTimeout::from(millis)) {
        Ok(_) | Err(Errno::EINTR) => {}
        Err(error) => return Err(error.into()),
    }
    Ok(polled
        .iter()
        .map(|fd| fd.revents().is_some_and(|events| !events.is_empty()))
        .collect())
}

/// Owns a child and stops its whole session when dropped: SIGTERM, then
/// SIGKILL after `grace`.
pub struct Guard(pub Child, Duration);

impl Guard {
    pub fn new(child: Child, grace: Duration) -> Self {
        Self(child, grace)
    }

    pub fn exited(&mut self) -> io::Result<Option<ExitStatus>> {
        self.0.try_wait()
    }

    /// Waits for exit. `Ok(None)` means the child is still running at the deadline.
    pub fn wait_until(&mut self, deadline: Instant) -> io::Result<Option<ExitStatus>> {
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(Some(status));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }

    pub fn stop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(None)) {
            return;
        }
        let group = Pid::from_raw(self.0.id() as i32);
        if !self.1.is_zero() {
            let _ = killpg(group, Signal::SIGTERM);
            if let Ok(Some(_)) = self.wait_until(Instant::now() + self.1) {
                return;
            }
        }
        let _ = killpg(group, Signal::SIGKILL);
        let _ = self.0.wait();
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_descriptors_are_closed_on_exec() {
        let terminal = terminal(None).unwrap();
        for descriptor in [&terminal.master, &terminal.slave] {
            let flags = nix::fcntl::fcntl(descriptor, nix::fcntl::FcntlArg::F_GETFD).unwrap();
            assert!(
                nix::fcntl::FdFlag::from_bits_truncate(flags)
                    .contains(nix::fcntl::FdFlag::FD_CLOEXEC)
            );
        }
    }
}
