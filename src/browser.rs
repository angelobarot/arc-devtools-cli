use anyhow::{anyhow, bail, Result};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

const DEFAULT_ARC_PORT: u16 = 9223;
pub const ARC_PORT_ENV: &str = "ARC_DEVTOOLS_PORT";
const ARC_BINARY: &str = "/Applications/Arc.app/Contents/MacOS/Arc";

/// Resolve the browser WebSocket URL: explicit `--ws-endpoint`, then the
/// `--user-data-dir` DevToolsActivePort escape hatch, otherwise Arc auto-management.
pub fn resolve_ws_url(ws_endpoint: Option<&str>, user_data_dir: Option<&str>) -> Result<String> {
    if let Some(ws) = ws_endpoint {
        return Ok(ws.to_string());
    }

    if let Some(dir) = user_data_dir {
        return read_devtools_active_port(Path::new(dir));
    }

    resolve_arc_ws()
}

fn arc_port() -> u16 {
    std::env::var(ARC_PORT_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_ARC_PORT)
}

/// Connect to (or start) Arc and return its browser-level WebSocket URL.
fn resolve_arc_ws() -> Result<String> {
    let port = arc_port();

    if let Some(ws) = probe_arc(port)? {
        ensure_page(port);
        return Ok(ws);
    }

    ensure_arc_debugging(port)?;

    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if let Some(ws) = probe_arc(port)? {
            ensure_page(port);
            return Ok(ws);
        }
        if Instant::now() > deadline {
            bail!(
                "Arc did not expose remote debugging on port {port} within 25s.\n\
                 Try launching it manually: {ARC_BINARY} --remote-debugging-port={port}"
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Launch Arc with remote debugging enabled. Arc has no in-app debug toggle and
/// ignores the flag when already running, so a running Arc is quit and relaunched.
#[cfg(target_os = "macos")]
fn ensure_arc_debugging(port: u16) -> Result<()> {
    if !Path::new(ARC_BINARY).exists() {
        bail!("Arc is not installed at {ARC_BINARY}");
    }

    if arc_running() {
        eprintln!("arc-devtools: restarting Arc to enable remote debugging on {port}...");
        let _ = std::process::Command::new("osascript")
            .args(["-e", "quit app \"Arc\""])
            .status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while arc_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
        }
        if arc_running() {
            bail!("Arc did not quit within 10s; close Arc manually and retry");
        }
    }

    std::process::Command::new(ARC_BINARY)
        .arg(format!("--remote-debugging-port={port}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("failed to launch Arc at {ARC_BINARY}: {e}"))?;

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ensure_arc_debugging(port: u16) -> Result<()> {
    bail!(
        "Automatic Arc launch is only supported on macOS. \
         Start Arc with --remote-debugging-port={port} and retry, \
         or pass --ws-endpoint."
    )
}

#[cfg(target_os = "macos")]
fn arc_running() -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", "Arc.app/Contents/MacOS/Arc"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Probe the debug port:
/// - `Ok(Some(ws))` — DevTools is up; returns its WebSocket URL
/// - `Ok(None)` — nothing reachable on the port (safe to launch Arc)
/// - `Err(..)` — something is listening but isn't DevTools (don't touch Arc)
fn probe_arc(port: u16) -> Result<Option<String>> {
    let body = match http_get(port, "/json/version") {
        Ok(body) => body,
        Err(_) => return Ok(None),
    };
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
        anyhow!("port {port} is in use but is not a DevTools endpoint; set ARC_DEVTOOLS_PORT to a free port")
    })?;
    match v.get("webSocketDebuggerUrl").and_then(|x| x.as_str()) {
        Some(ws) => Ok(Some(ws.to_string())),
        None => bail!("port {port} responded without a webSocketDebuggerUrl; set ARC_DEVTOOLS_PORT to a free port"),
    }
}

/// Best-effort: ensure at least one page target exists. A freshly launched Arc
/// has only internal/extension targets, which makes page-level commands fail
/// with "No page at index 0". Non-fatal.
fn ensure_page(port: u16) {
    let has_page = http_get(port, "/json/list")
        .ok()
        .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            v.as_array().map(|targets| {
                targets
                    .iter()
                    .any(|t| t.get("type").and_then(|x| x.as_str()) == Some("page"))
            })
        })
        .unwrap_or(true);

    if !has_page {
        let _ = http_request(port, "PUT", "/json/new?about:blank");
    }
}

fn http_get(port: u16, path: &str) -> Result<String> {
    http_request(port, "GET", path)
}

/// Minimal blocking HTTP/1.1 request to the local DevTools endpoint, returning
/// the response body. No external HTTP dependency.
///
/// DevTools requires HTTP/1.1 and derives `webSocketDebuggerUrl` from the Host
/// header — which must therefore include the port.
fn http_request(port: u16, method: &str, path: &str) -> Result<String> {
    const MAX_RESPONSE: usize = 4 * 1024 * 1024;

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(800))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;

    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes())?;

    // DevTools keeps the socket open, so stop as soon as Content-Length bytes
    // arrive; if there's no Content-Length, read until close/timeout.
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(h) = find_subslice(&raw, b"\r\n\r\n") {
            let body_start = h + 4;
            if let Some(len) = parse_content_length(&raw[..h]) {
                if raw.len() - body_start >= len {
                    return Ok(String::from_utf8_lossy(&raw[body_start..body_start + len])
                        .into_owned());
                }
            }
        }

        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() > MAX_RESPONSE {
                    bail!("HTTP response from port {port} exceeded {MAX_RESPONSE} bytes");
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        }
    }

    let text = String::from_utf8_lossy(&raw);
    Ok(text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

fn parse_content_length(headers: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(headers).ok()?;
    for line in text.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                return value.trim().parse().ok();
            }
        }
    }
    None
}

/// Read `DevToolsActivePort` file and construct the WebSocket URL.
/// Used only when `--user-data-dir` is supplied (Chrome escape hatch).
fn read_devtools_active_port(user_data_dir: &Path) -> Result<String> {
    let port_path = user_data_dir.join("DevToolsActivePort");

    let content = std::fs::read_to_string(&port_path).map_err(|_| {
        anyhow!(
            "Could not read DevToolsActivePort at {}\n\n\
             The browser must be running with remote debugging enabled for this \
             --user-data-dir.",
            port_path.display()
        )
    })?;

    let lines: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    if lines.len() < 2 {
        bail!(
            "Invalid DevToolsActivePort content: expected port and path, got: {:?}",
            content.trim()
        );
    }

    let port: u16 = lines[0]
        .parse()
        .map_err(|_| anyhow!("Invalid port '{}' in DevToolsActivePort", lines[0]))?;

    if port == 0 {
        bail!("Port 0 in DevToolsActivePort — the browser may not be running");
    }

    let path = lines[1];
    Ok(format!("ws://127.0.0.1:{port}{path}"))
}
