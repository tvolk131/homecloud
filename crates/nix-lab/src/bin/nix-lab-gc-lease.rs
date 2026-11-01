//! Lab-only demonstration of a coarse Nix GC exclusion lease, not a VM lifecycle service.
use std::{
    fs, io,
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err(io::Error::other(
            "usage: nix-lab-gc-lease GC_LOCK READY RELEASE",
        ));
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&args[0])?;
    lock.lock_shared()?;
    fs::write(&args[1], b"ready\n")?;
    let release = PathBuf::from(&args[2]);
    let start = Instant::now();
    while !release.exists() {
        if start.elapsed() > Duration::from_secs(300) {
            return Err(io::Error::other("lab lease deadline reached"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(lock);
    Ok(())
}
