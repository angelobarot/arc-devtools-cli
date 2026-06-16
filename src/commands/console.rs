use anyhow::Result;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::cdp::CdpClient;

pub struct ConsoleParams {
    /// Navigate here first, then capture the resulting console output.
    pub navigate: Option<String>,
    /// How long to capture console output for.
    pub duration_secs: u64,
    /// Only keep entries at this level or worse: all | warn | error.
    pub level: String,
}

struct Entry {
    level: String,
    text: String,
    location: Option<String>,
}

/// Capture console messages, uncaught exceptions, and browser log entries.
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
        let msg = match client.read_next_message(read_timeout).await {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(_) => break,
        };
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let p = match msg.get("params") {
            Some(p) => p,
            None => continue,
        };
        match method {
            "Runtime.consoleAPICalled" => {
                let level = normalize_level(p["type"].as_str().unwrap_or("log"));
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
                    level,
                    text,
                    location: stack_location(&p["stackTrace"]),
                });
            }
            "Runtime.exceptionThrown" => {
                let details = &p["exceptionDetails"];
                let text = details["exception"]["description"]
                    .as_str()
                    .or_else(|| details["text"].as_str())
                    .unwrap_or("Uncaught exception")
                    .to_string();
                let location = match (details["url"].as_str(), details["lineNumber"].as_i64()) {
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
                    location: entry["url"].as_str().map(|u| match entry["lineNumber"].as_i64() {
                        Some(l) => format!("{u}:{}", l + 1),
                        None => u.to_string(),
                    }),
                });
            }
            _ => {}
        }
    }

    let _ = client
        .send_to_target(session_id, "Log.disable", json!({}))
        .await;

    let min = min_rank(&params.level);
    let kept: Vec<&Entry> = entries.iter().filter(|e| level_rank(&e.level) >= min).collect();

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

fn render_remote_object(arg: &Value) -> String {
    if let Some(v) = arg.get("value") {
        return match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    }
    arg.get("description")
        .or_else(|| arg.get("unserializableValue"))
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
