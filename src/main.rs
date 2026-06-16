mod browser;
mod cdp;
mod client;
mod commands;
mod daemon;
mod friendly;
mod install;
mod protocol;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use serde_json::json;

use protocol::DaemonRequest;

#[derive(Parser)]
#[command(name = "arc-devtools", about = "Arc DevTools Protocol CLI")]
struct Cli {
    /// Explicit WebSocket endpoint (skips auto-connect)
    #[arg(long, global = true)]
    ws_endpoint: Option<String>,

    /// Chromium user data directory containing DevToolsActivePort (Chrome escape hatch)
    #[arg(long, global = true)]
    user_data_dir: Option<String>,

    /// Page index for page-level commands (0-based, from list-pages)
    #[arg(long, short, global = true)]
    page: Option<usize>,

    /// Target ID for page-level commands (stable across calls, from command output)
    #[arg(long, short, global = true)]
    target: Option<String>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Daemon idle timeout: 30m, 1h, 300s, or never
    #[arg(long, global = true, value_name = "value")]
    daemon_idle_timeout: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install the arc-devtools agent skill into opencode
    Install {
        /// Target skills directory (default: ~/.config/opencode/skills)
        #[arg(long)]
        dir: Option<String>,
    },

    /// List all open pages/tabs
    ListPages,

    /// Navigate to a URL, or go back/forward/reload
    Navigate {
        /// URL to navigate to
        url: Option<String>,
        #[arg(long)]
        back: bool,
        #[arg(long)]
        forward: bool,
        #[arg(long)]
        reload: bool,
    },

    /// Open a new page/tab
    NewPage {
        /// URL to open
        url: String,
    },

    /// Close a page/tab by index
    ClosePage {
        /// Page index (from list-pages)
        index: usize,
    },

    /// Bring a page to front
    SelectPage {
        /// Page index (from list-pages)
        index: usize,
    },

    /// Take a screenshot
    Screenshot {
        /// Save to file path (default: print base64 to stdout)
        #[arg(long, short)]
        output: Option<String>,
        /// Image format: png, jpeg, webp
        #[arg(long, default_value = "png")]
        format: String,
        /// Capture full scrollable page
        #[arg(long)]
        full_page: bool,
    },

    /// Evaluate a JavaScript expression
    Evaluate {
        /// JavaScript expression
        expression: String,
    },

    /// Click an element by CSS selector
    Click { selector: String },

    /// Fill an input field by CSS selector
    Fill { selector: String, value: String },

    /// Type text using keyboard (into currently focused element)
    TypeText { text: String },

    /// Press a key or key combination (e.g. Enter, Control+A)
    PressKey { key: String },

    /// Hover over an element by CSS selector
    Hover { selector: String },

    /// Take an accessibility tree snapshot
    Snapshot,

    /// Resize the page viewport
    Resize { width: u32, height: u32 },

    /// Wait for text to appear on the page
    WaitFor {
        text: String,
        #[arg(long, default_value_t = 30000)]
        timeout: u64,
    },

    /// Record the page to an MP4 video (requires ffmpeg)
    RecordVideo {
        /// Output file path (e.g. recording.mp4)
        #[arg(long, short)]
        output: String,
        /// Recording duration in seconds
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
        duration: u64,
        /// Target frames per second
        #[arg(long, default_value_t = 12, value_parser = clap::value_parser!(u32).range(1..=1000))]
        fps: u32,
        /// JPEG quality for screencast frames (1-100)
        #[arg(long, default_value_t = 80, value_parser = clap::value_parser!(u32).range(1..=100))]
        quality: u32,
        /// Max capture width (optional, caps resolution)
        #[arg(long)]
        width: Option<u32>,
        /// Max capture height (optional, caps resolution)
        #[arg(long)]
        height: Option<u32>,
    },
}

#[tokio::main]
async fn main() {
    // Internal daemon mode — invoked by spawn_daemon(), not by users
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("__daemon__") {
        let ws_url = args.get(2).expect("daemon requires ws_url argument");
        let initial_idle_timeout = match args.get(3) {
            Some(raw) => match protocol::DaemonIdleTimeout::parse(raw) {
                Ok(timeout) => Some(timeout),
                Err(e) => {
                    eprintln!("daemon error: invalid idle timeout '{raw}': {e}");
                    std::process::exit(1);
                }
            },
            None => None,
        };

        if let Err(e) = daemon::run_daemon(ws_url, initial_idle_timeout).await {
            eprintln!("daemon error: {e:#}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Build a DaemonRequest from parsed CLI args.
fn build_request(
    cli: &Cli,
    daemon_idle_timeout: Option<protocol::DaemonIdleTimeout>,
) -> DaemonRequest {
    let (command, args) = match &cli.command {
        Commands::Install { .. } => unreachable!("install is handled before build_request"),
        Commands::ListPages => ("list-pages", json!({})),
        Commands::Navigate {
            url,
            back,
            forward,
            reload,
        } => (
            "navigate",
            json!({"url": url, "back": back, "forward": forward, "reload": reload}),
        ),
        Commands::NewPage { url } => ("new-page", json!({"url": url})),
        Commands::ClosePage { index } => ("close-page", json!({"index": index})),
        Commands::SelectPage { index } => ("select-page", json!({"index": index})),
        Commands::Screenshot {
            output,
            format,
            full_page,
        } => (
            "screenshot",
            json!({"output": output, "format": format, "full_page": full_page}),
        ),
        Commands::Evaluate { expression } => ("evaluate", json!({"expression": expression})),
        Commands::Click { selector } => ("click", json!({"selector": selector})),
        Commands::Fill { selector, value } => {
            ("fill", json!({"selector": selector, "value": value}))
        }
        Commands::TypeText { text } => ("type-text", json!({"text": text})),
        Commands::PressKey { key } => ("press-key", json!({"key": key})),
        Commands::Hover { selector } => ("hover", json!({"selector": selector})),
        Commands::Snapshot => ("snapshot", json!({})),
        Commands::Resize { width, height } => ("resize", json!({"width": width, "height": height})),
        Commands::WaitFor { text, timeout } => {
            ("wait-for", json!({"text": text, "timeout": timeout}))
        }
        Commands::RecordVideo {
            output,
            duration,
            fps,
            quality,
            width,
            height,
        } => (
            "record-video",
            json!({
                "output": output,
                "duration": duration,
                "fps": fps,
                "quality": quality,
                "width": width,
                "height": height,
            }),
        ),
    };

    DaemonRequest {
        command: command.to_string(),
        args,
        page: cli.page,
        target: cli.target.clone(),
        json_output: cli.json,
        daemon_idle_timeout,
        ws_endpoint: cli.ws_endpoint.clone(),
        user_data_dir: cli.user_data_dir.clone(),
    }
}

fn print_response(resp: &protocol::DaemonResponse) {
    if resp.success {
        if !resp.output.is_empty() {
            print!("{}", resp.output);
            if !resp.output.ends_with('\n') {
                println!();
            }
        }
    } else {
        eprintln!("error: {}", resp.error);
        std::process::exit(1);
    }
}

fn resolve_daemon_idle_timeout(
    cli_value: Option<&str>,
) -> Result<Option<protocol::DaemonIdleTimeout>> {
    if let Some(value) = cli_value {
        return Ok(Some(protocol::DaemonIdleTimeout::parse(value)?));
    }

    match std::env::var(protocol::DAEMON_IDLE_TIMEOUT_ENV) {
        Ok(value) => Ok(Some(protocol::DaemonIdleTimeout::parse(&value).map_err(
            |e| {
                anyhow!(
                    "invalid {} value '{}': {e}",
                    protocol::DAEMON_IDLE_TIMEOUT_ENV,
                    value
                )
            },
        )?)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(anyhow!(
            "failed to read {}: {e}",
            protocol::DAEMON_IDLE_TIMEOUT_ENV
        )),
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    // `install` doesn't touch the browser — handle it before connecting.
    if let Commands::Install { dir } = &cli.command {
        return install::install_skill(dir.as_deref());
    }

    let daemon_idle_timeout = resolve_daemon_idle_timeout(cli.daemon_idle_timeout.as_deref())?;

    let ws_url =
        browser::resolve_ws_url(cli.ws_endpoint.as_deref(), cli.user_data_dir.as_deref())?;

    let request = build_request(&cli, daemon_idle_timeout);

    if let Ok(resp) = client::send_to_daemon(&request).await {
        if resp.success {
            print_response(&resp);
            return Ok(());
        }

        if cdp::is_retryable_connection_error_message(&resp.error) {
            eprintln!("Warning: daemon browser connection was stale, running directly");
            return run_direct_and_print(&cli, &ws_url).await;
        }

        print_response(&resp);
        return Ok(());
    }

    // Daemon not running — try to spawn it, then fall back directly if startup fails.
    if let Err(e) = client::spawn_daemon(&ws_url, daemon_idle_timeout) {
        eprintln!("Warning: failed to spawn daemon ({e}), running directly");
        return run_direct_and_print(&cli, &ws_url).await;
    }

    if let Err(e) = client::wait_for_daemon().await {
        eprintln!("Warning: daemon failed to become ready ({e}), running directly");
        return run_direct_and_print(&cli, &ws_url).await;
    }

    // Retry via daemon.
    match client::send_to_daemon(&request).await {
        Ok(resp) => {
            if !resp.success && cdp::is_retryable_connection_error_message(&resp.error) {
                eprintln!(
                    "Warning: daemon browser connection was stale after startup, running directly"
                );
                return run_direct_and_print(&cli, &ws_url).await;
            }

            print_response(&resp);
            Ok(())
        }
        Err(e) => {
            eprintln!("Warning: daemon unavailable ({e}), running directly");
            run_direct_and_print(&cli, &ws_url).await
        }
    }
}

async fn run_direct_and_print(cli: &Cli, ws_url: &str) -> Result<()> {
    let output = run_direct(cli, ws_url).await?;
    if !output.is_empty() {
        print!("{}", output);
        if !output.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

/// Direct execution without daemon (fallback).
async fn run_direct(cli: &Cli, ws_url: &str) -> Result<String> {
    let mut client = cdp::CdpClient::connect(ws_url).await?;

    let is_browser = matches!(
        cli.command,
        Commands::ListPages
            | Commands::NewPage { .. }
            | Commands::ClosePage { .. }
            | Commands::SelectPage { .. }
    );

    if is_browser {
        return match &cli.command {
            Commands::ListPages => commands::pages::list_pages(&mut client, cli.json).await,
            Commands::NewPage { url } => commands::pages::new_page(&mut client, url).await,
            Commands::ClosePage { index } => commands::pages::close_page(&mut client, *index).await,
            Commands::SelectPage { index } => {
                commands::pages::select_page(&mut client, *index).await
            }
            _ => unreachable!(),
        };
    }

    let target = client.resolve_page(cli.target.as_deref(), cli.page).await?;
    let target_id = target.target_id.clone();
    let session_id = client.attach_to_target(&target_id).await?;

    let result = match &cli.command {
        Commands::Navigate {
            url,
            back,
            forward,
            reload,
        } => {
            commands::navigate::navigate(
                &mut client,
                &session_id,
                url.as_deref(),
                *back,
                *forward,
                *reload,
            )
            .await
        }
        Commands::Screenshot {
            output,
            format,
            full_page,
        } => {
            commands::screenshot::take_screenshot(
                &mut client,
                &session_id,
                output.as_deref(),
                format,
                *full_page,
            )
            .await
        }
        Commands::Evaluate { expression } => {
            commands::evaluate::evaluate(&mut client, &session_id, expression, cli.json).await
        }
        Commands::Click { selector } => {
            commands::input::click(&mut client, &session_id, selector).await
        }
        Commands::Fill { selector, value } => {
            commands::input::fill(&mut client, &session_id, selector, value).await
        }
        Commands::TypeText { text } => {
            commands::input::type_text(&mut client, &session_id, text).await
        }
        Commands::PressKey { key } => {
            commands::input::press_key(&mut client, &session_id, key).await
        }
        Commands::Hover { selector } => {
            commands::input::hover(&mut client, &session_id, selector).await
        }
        Commands::Snapshot => {
            commands::snapshot::take_snapshot(&mut client, &session_id, cli.json).await
        }
        Commands::Resize { width, height } => {
            commands::pages::resize(&mut client, &session_id, *width, *height).await
        }
        Commands::WaitFor { text, timeout } => {
            commands::pages::wait_for(&mut client, &session_id, text, *timeout).await
        }
        Commands::RecordVideo {
            output,
            duration,
            fps,
            quality,
            width,
            height,
        } => {
            let params = commands::record_video::RecordVideoParams {
                output: output.clone(),
                duration_secs: *duration,
                fps: *fps,
                quality: *quality,
                max_width: *width,
                max_height: *height,
            };
            commands::record_video::record_video(&mut client, &session_id, &params).await
        }
        _ => unreachable!(),
    };

    let _ = client.detach_from_target(&session_id).await;
    let name = friendly::to_friendly(&target_id);
    result.map(|output| format!("{output}\n[target:{name}]"))
}
