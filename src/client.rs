use anyhow::{bail, Result};
use std::fs::OpenOptions;
use std::process::Stdio;
use std::time::Duration;
use tokio::net::UnixStream;

use crate::protocol::*;

/// Try to send a request to the daemon. Returns error if daemon is not running.
pub async fn send_to_daemon(request: &DaemonRequest) -> Result<DaemonResponse> {
    let sock = socket_path();
    let mut stream = UnixStream::connect(&sock).await?;

    let req_bytes = serde_json::to_vec(request)?;
    write_msg(&mut stream, &req_bytes).await?;

    let resp_bytes = read_msg(&mut stream).await?;
    let response: DaemonResponse = serde_json::from_slice(&resp_bytes)?;
    Ok(response)
}

/// Spawn the daemon process in the background.
pub fn spawn_daemon(ws_url: &str, idle_timeout: Option<DaemonIdleTimeout>) -> Result<()> {
    let exe = std::env::current_exe()?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())?;
    let mut command = std::process::Command::new(&exe);
    command
        .arg("__daemon__")
        .arg(ws_url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    if let Some(timeout) = idle_timeout {
        command.arg(timeout.to_string());
    }

    command.spawn()?;
    Ok(())
}

/// Wait for the daemon socket to become available.
pub async fn wait_for_daemon() -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::time::Instant::now() > deadline {
            bail!("Daemon failed to start within 5 seconds");
        }
        if UnixStream::connect(socket_path()).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
