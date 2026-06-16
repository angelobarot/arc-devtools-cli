use anyhow::{anyhow, Result};
use tokio::net::UnixListener;

use crate::cdp::CdpClient;
use crate::commands;
use crate::friendly;
use crate::protocol::*;

pub async fn run_daemon(
    ws_url: &str,
    initial_idle_timeout: Option<DaemonIdleTimeout>,
) -> Result<()> {
    let mut client = CdpClient::connect(ws_url).await?;
    let mut idle_timeout = initial_idle_timeout.unwrap_or(DaemonIdleTimeout::DEFAULT);

    // Clean up stale socket
    let sock = socket_path();
    let _ = std::fs::remove_file(&sock);

    // Write PID
    std::fs::write(pid_path(), std::process::id().to_string())?;

    let listener = UnixListener::bind(&sock)?;

    // Signal readiness by socket existence (it's already bound)
    loop {
        match accept_with_idle_timeout(&listener, idle_timeout).await {
            Ok(Some((mut stream, _))) => {
                let req_bytes = match read_msg(&mut stream).await {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("daemon: read error: {e}");
                        continue;
                    }
                };

                let request: DaemonRequest = match serde_json::from_slice(&req_bytes) {
                    Ok(r) => r,
                    Err(e) => {
                        let resp = DaemonResponse {
                            success: false,
                            output: String::new(),
                            error: format!("Invalid request: {e}"),
                        };
                        let _ = write_msg(&mut stream, &serde_json::to_vec(&resp).unwrap()).await;
                        continue;
                    }
                };

                if let Some(next_idle_timeout) = request.daemon_idle_timeout {
                    idle_timeout = next_idle_timeout;
                }

                let response = handle_request(&mut client, &request).await;

                if let Ok(resp_bytes) = serde_json::to_vec(&response) {
                    let _ = write_msg(&mut stream, &resp_bytes).await;
                }
            }
            Ok(None) => {
                // Idle timeout — exit
                break;
            }
            Err(e) => {
                eprintln!("daemon: accept error: {e}");
            }
        }
    }

    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(pid_path());
    Ok(())
}

async fn accept_with_idle_timeout(
    listener: &UnixListener,
    idle_timeout: DaemonIdleTimeout,
) -> std::result::Result<
    Option<(tokio::net::UnixStream, tokio::net::unix::SocketAddr)>,
    std::io::Error,
> {
    if let Some(duration) = idle_timeout.as_duration() {
        return match tokio::time::timeout(duration, listener.accept()).await {
            Ok(accept_result) => accept_result.map(Some),
            Err(_) => Ok(None),
        };
    }

    listener.accept().await.map(Some)
}

async fn handle_request(client: &mut CdpClient, req: &DaemonRequest) -> DaemonResponse {
    match execute_command(client, req).await {
        Ok(output) => success_response(output),
        Err(e) if crate::cdp::is_retryable_connection_error(&e) => {
            match reconnect_client(req).await {
                Ok(reconnected) => {
                    *client = reconnected;
                    match execute_command(client, req).await {
                        Ok(output) => success_response(output),
                        Err(retry_error) => error_response(retry_error),
                    }
                }
                Err(reconnect_error) => error_response(reconnect_error.context(format!(
                    "failed to reconnect daemon browser session after retryable error: {e:#}"
                ))),
            }
        }
        Err(e) => error_response(e),
    }
}

fn success_response(output: String) -> DaemonResponse {
    DaemonResponse {
        success: true,
        output,
        error: String::new(),
    }
}

fn error_response(error: anyhow::Error) -> DaemonResponse {
    DaemonResponse {
        success: false,
        output: String::new(),
        error: format!("{error:#}"),
    }
}

async fn reconnect_client(req: &DaemonRequest) -> Result<CdpClient> {
    let ws_url =
        crate::browser::resolve_ws_url(req.ws_endpoint.as_deref(), req.user_data_dir.as_deref())?;
    CdpClient::connect(&ws_url).await
}

fn is_browser_level(cmd: &str) -> bool {
    matches!(
        cmd,
        "list-pages" | "new-page" | "close-page" | "select-page"
    )
}

async fn execute_command(client: &mut CdpClient, req: &DaemonRequest) -> Result<String> {
    let args = &req.args;
    let cmd = req.command.as_str();

    if is_browser_level(cmd) {
        return match cmd {
            "list-pages" => commands::pages::list_pages(client, req.json_output).await,
            "new-page" => {
                let url = args["url"].as_str().ok_or(anyhow!("url required"))?;
                commands::pages::new_page(client, url).await
            }
            "close-page" => {
                let index = args["index"].as_u64().ok_or(anyhow!("index required"))? as usize;
                commands::pages::close_page(client, index).await
            }
            "select-page" => {
                let index = args["index"].as_u64().ok_or(anyhow!("index required"))? as usize;
                commands::pages::select_page(client, index).await
            }
            _ => unreachable!(),
        };
    }

    // Page-level: resolve and attach to target
    let target = client.resolve_page(req.target.as_deref(), req.page).await?;
    let target_id = target.target_id.clone();
    let session_id = client.attach_to_target(&target_id).await?;

    let result = match cmd {
        "navigate" => {
            commands::navigate::navigate(
                client,
                &session_id,
                args["url"].as_str(),
                args["back"].as_bool().unwrap_or(false),
                args["forward"].as_bool().unwrap_or(false),
                args["reload"].as_bool().unwrap_or(false),
            )
            .await
        }
        "screenshot" => {
            commands::screenshot::take_screenshot(
                client,
                &session_id,
                args["output"].as_str(),
                args["format"].as_str().unwrap_or("png"),
                args["full_page"].as_bool().unwrap_or(false),
            )
            .await
        }
        "evaluate" => {
            let expr = args["expression"]
                .as_str()
                .ok_or(anyhow!("expression required"))?;
            commands::evaluate::evaluate(client, &session_id, expr, req.json_output).await
        }
        "click" => {
            let sel = args["selector"]
                .as_str()
                .ok_or(anyhow!("selector required"))?;
            commands::input::click(client, &session_id, sel).await
        }
        "fill" => {
            let sel = args["selector"]
                .as_str()
                .ok_or(anyhow!("selector required"))?;
            let val = args["value"].as_str().ok_or(anyhow!("value required"))?;
            commands::input::fill(client, &session_id, sel, val).await
        }
        "type-text" => {
            let text = args["text"].as_str().ok_or(anyhow!("text required"))?;
            commands::input::type_text(client, &session_id, text).await
        }
        "press-key" => {
            let key = args["key"].as_str().ok_or(anyhow!("key required"))?;
            commands::input::press_key(client, &session_id, key).await
        }
        "hover" => {
            let sel = args["selector"]
                .as_str()
                .ok_or(anyhow!("selector required"))?;
            commands::input::hover(client, &session_id, sel).await
        }
        "snapshot" => commands::snapshot::take_snapshot(client, &session_id, req.json_output).await,
        "resize" => {
            let w = args["width"].as_u64().ok_or(anyhow!("width required"))? as u32;
            let h = args["height"].as_u64().ok_or(anyhow!("height required"))? as u32;
            commands::pages::resize(client, &session_id, w, h).await
        }
        "wait-for" => {
            let text = args["text"].as_str().ok_or(anyhow!("text required"))?;
            let timeout = args["timeout"].as_u64().unwrap_or(30000);
            commands::pages::wait_for(client, &session_id, text, timeout).await
        }
        "record-video" => {
            let output = args["output"]
                .as_str()
                .ok_or(anyhow!("output required"))?;
            let params = commands::record_video::RecordVideoParams {
                output: output.to_string(),
                duration_secs: args["duration"].as_u64().unwrap_or(5),
                fps: args["fps"].as_u64().unwrap_or(12) as u32,
                quality: args["quality"].as_u64().unwrap_or(80) as u32,
                max_width: args["width"].as_u64().map(|v| v as u32),
                max_height: args["height"].as_u64().map(|v| v as u32),
            };
            commands::record_video::record_video(client, &session_id, &params).await
        }
        "network" => {
            let params = commands::network::NetworkParams {
                navigate: args["navigate"].as_str().map(str::to_string),
                duration_secs: args["duration"].as_u64().unwrap_or(5),
                filter: args["filter"].as_str().map(str::to_string),
            };
            commands::network::network(client, &session_id, &params, req.json_output).await
        }
        "console" => {
            let params = commands::console::ConsoleParams {
                navigate: args["navigate"].as_str().map(str::to_string),
                duration_secs: args["duration"].as_u64().unwrap_or(5),
                level: args["level"].as_str().unwrap_or("all").to_string(),
            };
            commands::console::console(client, &session_id, &params, req.json_output).await
        }
        "performance" => {
            commands::performance::performance(
                client,
                &session_id,
                args["navigate"].as_str(),
                req.json_output,
            )
            .await
        }
        _ => Err(anyhow!("Unknown command: {cmd}")),
    };

    let _ = client.detach_from_target(&session_id).await;

    // Append target ID so the caller can pin subsequent commands to this page
    let name = friendly::to_friendly(&target_id);
    result.map(|output| format!("{output}\n[target:{name}]"))
}
