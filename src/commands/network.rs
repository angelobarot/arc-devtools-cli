use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::cdp::CdpClient;

pub struct NetworkParams {
    /// Navigate here first, then capture the resulting requests.
    pub navigate: Option<String>,
    /// How long to capture network activity for.
    pub duration_secs: u64,
    /// Only include requests whose URL contains this substring.
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

    // Fire-and-forget so the capture loop (not send_raw) consumes the events.
    if let Some(url) = &params.navigate {
        client
            .send_fire_and_forget("Page.navigate", json!({"url": url}), Some(session_id))
            .await?;
    }

    let mut reqs: HashMap<String, Request> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

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
            "Network.requestWillBeSent" => {
                let id = p["requestId"].as_str().unwrap_or("").to_string();
                if id.is_empty() {
                    continue;
                }
                if !reqs.contains_key(&id) {
                    order.push(id.clone());
                }
                reqs.insert(
                    id,
                    Request {
                        method: p["request"]["method"].as_str().unwrap_or("").to_string(),
                        url: p["request"]["url"].as_str().unwrap_or("").to_string(),
                        status: None,
                        resource_type: p["type"].as_str().map(str::to_string),
                        encoded_len: 0.0,
                        start_ms: p["timestamp"].as_f64().unwrap_or(0.0) * 1000.0,
                        end_ms: None,
                        failed: None,
                    },
                );
            }
            "Network.responseReceived" => {
                if let Some(r) = reqs.get_mut(p["requestId"].as_str().unwrap_or("")) {
                    r.status = p["response"]["status"].as_i64();
                    if let Some(t) = p["type"].as_str() {
                        r.resource_type = Some(t.to_string());
                    }
                }
            }
            "Network.loadingFinished" => {
                if let Some(r) = reqs.get_mut(p["requestId"].as_str().unwrap_or("")) {
                    r.end_ms = Some(p["timestamp"].as_f64().unwrap_or(0.0) * 1000.0);
                    r.encoded_len = p["encodedDataLength"].as_f64().unwrap_or(r.encoded_len);
                }
            }
            "Network.loadingFailed" => {
                if let Some(r) = reqs.get_mut(p["requestId"].as_str().unwrap_or("")) {
                    r.end_ms = Some(p["timestamp"].as_f64().unwrap_or(0.0) * 1000.0);
                    r.failed = Some(p["errorText"].as_str().unwrap_or("failed").to_string());
                }
            }
            _ => {}
        }
    }

    let _ = client
        .send_to_target(session_id, "Network.disable", json!({}))
        .await;

    let filtered: Vec<&Request> = order
        .iter()
        .filter_map(|id| reqs.get(id))
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
                    "encodedBytes": r.encoded_len,
                    "durationMs": r.end_ms.map(|e| e - r.start_ms),
                    "error": r.failed,
                })
            })
            .collect();
        return Ok(serde_json::to_string_pretty(&json!({"requests": arr}))?);
    }

    if filtered.is_empty() {
        return Ok("No network requests captured.".to_string());
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{:<6} {:<6} {:<11} {:>9} {:>7}  URL\n",
        "STATUS", "METHOD", "TYPE", "SIZE", "TIME"
    ));
    let mut total_bytes = 0.0;
    for r in &filtered {
        let status = match (&r.failed, r.status) {
            (Some(_), _) => "ERR".to_string(),
            (None, Some(s)) => s.to_string(),
            (None, None) => "—".to_string(),
        };
        let time = match r.end_ms {
            Some(e) => format!("{}ms", (e - r.start_ms).round() as i64),
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

fn human_bytes(n: f64) -> String {
    if n <= 0.0 {
        return "0B".to_string();
    }
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{}B", v.round() as i64)
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}
