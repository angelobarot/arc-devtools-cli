use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::cdp::CdpClient;
use crate::commands::format::human_bytes;

pub struct NetworkParams {
    pub navigate: Option<String>,
    pub duration_secs: u64,
    pub filter: Option<String>,
}

struct Request {
    method: String,
    url: String,
    status: Option<i64>,
    resource_type: Option<String>,
    encoded_len: f64,
    start_ms: f64,
    end_ms: Option<f64>,
    failed: Option<String>,
}

/// Capture the network request waterfall via the CDP Network domain.
pub async fn network(
    client: &mut CdpClient,
    session_id: &str,
    params: &NetworkParams,
    json_output: bool,
) -> Result<String> {
    client
        .send_to_target(session_id, "Network.enable", json!({}))
        .await?;

    // Always disable the domain, even if capture fails partway.
    let captured = capture(client, session_id, params).await;
    let _ = client
        .send_to_target(session_id, "Network.disable", json!({}))
        .await;
    let entries = captured?;

    let filtered: Vec<&Request> = entries
        .iter()
        .filter(|r| {
            params
                .filter
                .as_ref()
                .map(|f| r.url.contains(f.as_str()))
                .unwrap_or(true)
        })
        .collect();

    if json_output {
        let arr: Vec<Value> = filtered
            .iter()
            .map(|r| {
                json!({
                    "method": r.method,
                    "url": r.url,
                    "status": r.status,
                    "type": r.resource_type,
                    "encodedBytes": r.encoded_len.round() as i64,
                    "durationMs": duration_ms(r).map(|d| d.round() as i64),
                    "error": r.failed,
                })
            })
            .collect();
        return Ok(serde_json::to_string_pretty(&json!({"requests": arr}))?);
    }

    if filtered.is_empty() {
        return Ok("No network requests captured.".to_string());
    }

    let mut out = format!(
        "{:<6} {:<6} {:<11} {:>9} {:>7}  URL\n",
        "STATUS", "METHOD", "TYPE", "SIZE", "TIME"
    );
    let mut total_bytes = 0.0;
    for r in &filtered {
        let status = match (&r.failed, r.status) {
            (Some(_), _) => "ERR".to_string(),
            (None, Some(s)) => s.to_string(),
            (None, None) => "—".to_string(),
        };
        let time = match duration_ms(r) {
            Some(d) => format!("{}ms", d.round() as i64),
            None => "—".to_string(),
        };
        total_bytes += r.encoded_len;
        out.push_str(&format!(
            "{:<6} {:<6} {:<11} {:>9} {:>7}  {}\n",
            status,
            r.method,
            r.resource_type.as_deref().unwrap_or("—"),
            human_bytes(r.encoded_len),
            time,
            r.url,
        ));
        if let Some(err) = &r.failed {
            out.push_str(&format!("       └─ {err}\n"));
        }
    }
    out.push_str(&format!(
        "\n{} request(s), {} transferred",
        filtered.len(),
        human_bytes(total_bytes)
    ));
    Ok(out)
}

/// Drive the page and collect request events until the capture window closes.
/// Returns requests in arrival order, including redirect hops.
async fn capture(
    client: &mut CdpClient,
    session_id: &str,
    params: &NetworkParams,
) -> Result<Vec<Request>> {
    // Fire-and-forget: send_to_target would consume the events this loop needs.
    if let Some(url) = &params.navigate {
        client
            .send_fire_and_forget("Page.navigate", json!({"url": url}), Some(session_id))
            .await?;
    }

    let mut entries: Vec<Request> = Vec::new();
    let mut current: HashMap<String, usize> = HashMap::new();

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
            "Network.requestWillBeSent" => {
                let id = p["requestId"].as_str().unwrap_or("").to_string();
                if id.is_empty() {
                    continue;
                }
                let ts = p["timestamp"].as_f64().unwrap_or(0.0) * 1000.0;
                // A redirect reuses the requestId; finalize the prior hop first.
                if let Some(redirect) = p.get("redirectResponse") {
                    if let Some(&i) = current.get(&id) {
                        entries[i].status = redirect["status"].as_i64();
                        entries[i].end_ms = Some(ts);
                    }
                }
                entries.push(Request {
                    method: p["request"]["method"].as_str().unwrap_or("").to_string(),
                    url: p["request"]["url"].as_str().unwrap_or("").to_string(),
                    status: None,
                    resource_type: p["type"].as_str().map(str::to_string),
                    encoded_len: 0.0,
                    start_ms: ts,
                    end_ms: None,
                    failed: None,
                });
                current.insert(id, entries.len() - 1);
            }
            "Network.responseReceived" => {
                if let Some(&i) = current.get(p["requestId"].as_str().unwrap_or("")) {
                    entries[i].status = p["response"]["status"].as_i64();
                    if let Some(t) = p["type"].as_str() {
                        entries[i].resource_type = Some(t.to_string());
                    }
                }
            }
            "Network.loadingFinished" => {
                if let Some(&i) = current.get(p["requestId"].as_str().unwrap_or("")) {
                    entries[i].end_ms = Some(p["timestamp"].as_f64().unwrap_or(0.0) * 1000.0);
                    entries[i].encoded_len =
                        p["encodedDataLength"].as_f64().unwrap_or(entries[i].encoded_len);
                }
            }
            "Network.loadingFailed" => {
                if let Some(&i) = current.get(p["requestId"].as_str().unwrap_or("")) {
                    entries[i].end_ms = Some(p["timestamp"].as_f64().unwrap_or(0.0) * 1000.0);
                    entries[i].failed =
                        Some(p["errorText"].as_str().unwrap_or("failed").to_string());
                }
            }
            _ => {}
        }
    }
    Ok(entries)
}

/// Request duration in ms, only when both endpoints are known and sane.
fn duration_ms(r: &Request) -> Option<f64> {
    r.end_ms
        .map(|e| e - r.start_ms)
        .filter(|&d| d >= 0.0)
}
