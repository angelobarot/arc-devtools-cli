use anyhow::Result;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::cdp::CdpClient;

pub struct ConsoleParams {
    pub navigate: Option<String>,
    pub duration_secs: u64,
    pub level: String,
}

struct Entry {
    level: String,
    text: String,
    location: Option<String>,
}

/// Capture console messages, uncaught exceptions, and browser log entries.
/// Enables the Runtime and Log domains for the capture window.
pub async fn console(
    client: &mut CdpClient,
    session_id: &str,
    params: &ConsoleParams,
    json_output: bool,
) -> Result<String> {
    client
        .send_to_target(session_id, "Runtime.enable", json!({}))
        .await?;
    client
        .send_to_target(session_id, "Log.enable", json!({}))
        .await?;

    // Always disable the domains, even if capture fails partway.
    let captured = capture(client, session_id, params).await;
    let _ = client
        .send_to_target(session_id, "Log.disable", json!({}))
        .await;
    let _ = client
        .send_to_target(session_id, "Runtime.disable", json!({}))
        .await;
    let entries = captured?;

    let min = min_rank(&params.level);
    let kept: Vec<&Entry> = entries
        .iter()
        .filter(|e| level_rank(&e.level) >= min)
        .collect();

    if json_output {
        let arr: Vec<Value> = kept
            .iter()
            .map(|e| json!({"level": e.level, "text": e.text, "location": e.location}))
            .collect();
        return Ok(serde_json::to_string_pretty(&json!({"messages": arr}))?);
    }

    if kept.is_empty() {
        return Ok("No console output captured.".to_string());
    }

    let mut out = String::new();
    for e in &kept {
        match &e.location {
            Some(loc) => out.push_str(&format!("[{}] {}  ({loc})\n", e.level, e.text)),
            None => out.push_str(&format!("[{}] {}\n", e.level, e.text)),
        }
    }
    out.push_str(&format!("\n{} message(s)", kept.len()));
    Ok(out)
}

async fn capture(
    client: &mut CdpClient,
    session_id: &str,
    params: &ConsoleParams,
) -> Result<Vec<Entry>> {
    // Fire-and-forget: send_to_target would consume the events this loop needs.
    if let Some(url) = &params.navigate {
        client
            .send_fire_and_forget("Page.navigate", json!({"url": url}), Some(session_id))
            .await?;
    }

    let mut entries: Vec<Entry> = Vec::new();
    let start = Instant::now();
    let duration = Duration::from_secs(params.duration_secs);
    while start.elapsed() < duration {
        let read_timeout = (duration - start.elapsed()).min(Duration::from_millis(200));
        let msg = match client.read_next_message(read_timeout).await? {
            Some(m) => m,
            None => continue,
        };
        if msg.get("sessionId").and_then(|v| v.as_str()) != Some(session_id) {
            continue;
        }
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let p = match msg.get("params") {
            Some(p) => p,
            None => continue,
        };
        match method {
            "Runtime.consoleAPICalled" => {
                let text = p["args"]
                    .as_array()
                    .map(|args| {
                        args.iter()
                            .map(render_remote_object)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                entries.push(Entry {
                    level: normalize_level(p["type"].as_str().unwrap_or("log")),
                    text,
                    location: stack_location(&p["stackTrace"]),
                });
            }
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                let text = d["exception"]["description"]
                    .as_str()
                    .or_else(|| d["text"].as_str())
                    .unwrap_or("Uncaught exception")
                    .to_string();
                let location = match (d["url"].as_str(), d["lineNumber"].as_i64()) {
                    (Some(u), Some(l)) => Some(format!("{u}:{}", l + 1)),
                    _ => None,
                };
                entries.push(Entry {
                    level: "error".to_string(),
                    text,
                    location,
                });
            }
            "Log.entryAdded" => {
                let entry = &p["entry"];
                entries.push(Entry {
                    level: normalize_level(entry["level"].as_str().unwrap_or("info")),
                    text: entry["text"].as_str().unwrap_or("").to_string(),
                    location: entry["url"]
                        .as_str()
                        .map(|u| match entry["lineNumber"].as_i64() {
                            Some(l) => format!("{u}:{}", l + 1),
                            None => u.to_string(),
                        }),
                });
            }
            _ => {}
        }
    }
    Ok(entries)
}

/// Render a CDP RemoteObject for display: primitives by value, objects/arrays
/// from their preview, otherwise the description or type name.
fn render_remote_object(arg: &Value) -> String {
    if let Some(v) = arg.get("value") {
        return match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    }
    if let Some(props) = arg["preview"]["properties"].as_array() {
        let inner = props
            .iter()
            .map(|p| {
                format!(
                    "{}: {}",
                    p["name"].as_str().unwrap_or(""),
                    p["value"].as_str().unwrap_or("…")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return if arg["subtype"].as_str() == Some("array") {
            format!("[{inner}]")
        } else {
            format!("{{{inner}}}")
        };
    }
    arg.get("description")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| arg["type"].as_str().unwrap_or("?").to_string())
}

fn stack_location(stack: &Value) -> Option<String> {
    let frame = stack["callFrames"].as_array()?.first()?;
    let url = frame["url"].as_str().filter(|u| !u.is_empty())?;
    let line = frame["lineNumber"].as_i64().unwrap_or(0) + 1;
    Some(format!("{url}:{line}"))
}

fn normalize_level(level: &str) -> String {
    match level {
        "warning" | "warn" => "warning",
        "error" | "assert" => "error",
        "debug" | "verbose" => "debug",
        "info" => "info",
        _ => "log",
    }
    .to_string()
}

fn level_rank(level: &str) -> u8 {
    match level {
        "error" => 3,
        "warning" => 2,
        _ => 1,
    }
}

fn min_rank(level: &str) -> u8 {
    match level {
        "error" => 3,
        "warn" | "warning" => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{level_rank, min_rank, normalize_level};

    #[test]
    fn normalizes_levels() {
        assert_eq!(normalize_level("warn"), "warning");
        assert_eq!(normalize_level("warning"), "warning");
        assert_eq!(normalize_level("assert"), "error");
        assert_eq!(normalize_level("verbose"), "debug");
        assert_eq!(normalize_level("anything-else"), "log");
    }

    #[test]
    fn level_filtering_thresholds() {
        // --level error keeps only errors
        assert!(level_rank("error") >= min_rank("error"));
        assert!(level_rank("warning") < min_rank("error"));
        // --level warn keeps warnings and errors, not logs
        assert!(level_rank("warning") >= min_rank("warn"));
        assert!(level_rank("error") >= min_rank("warn"));
        assert!(level_rank("log") < min_rank("warn"));
        // --level all keeps everything
        assert!(level_rank("log") >= min_rank("all"));
    }
}
