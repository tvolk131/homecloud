use anyhow::{Result, ensure};
use homecloud_host::{HostResponse, observer};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        println!(
            "Homecloud read-only privileged host observer\n\nReads one Inventory or Observe JSON request from stdin.\nNo paths, commands, or mutation operations are accepted."
        );
        return Ok(());
    }
    ensure!(args.is_empty(), "The host observer accepts no arguments");
    let response = observer::serve()
        .await
        .unwrap_or_else(|error| HostResponse::Error(format!("{error:#}")));
    serde_json::to_writer(std::io::stdout().lock(), &response)?;
    Ok(())
}
