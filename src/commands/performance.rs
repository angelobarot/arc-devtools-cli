use anyhow::Result;
use serde_json::{json, Value};

use crate::cdp::CdpClient;

const SNAPSHOT_JS: &str = r#"(() => {
  const nav = performance.getEntriesByType('navigation')[0] || {};
  const paint = {};
  performance.getEntriesByType('paint').forEach(p => { paint[p.name] = p.startTime; });
  const mem = performance.memory || {};
  return JSON.stringify({
    ttfb: nav.responseStart,
    domInteractive: nav.domInteractive,
    domContentLoaded: nav.domContentLoadedEventEnd,
    load: nav.loadEventEnd,
    firstPaint: paint['first-paint'],
    firstContentfulPaint: paint['first-contentful-paint'],
    transferSize: nav.transferSize,
    resources: performance.getEntriesByType('resource').length,
    jsHeapUsed: mem.usedJSHeapSize,
    jsHeapTotal: mem.totalJSHeapSize,
    domNodes: document.getElementsByTagName('*').length
  });
})()"#;

/// One-shot performance snapshot: navigation/paint timings + JS heap + DOM size.
pub async fn performance(
    client: &mut CdpClient,
    session_id: &str,
    navigate: Option<&str>,
    json_output: bool,
) -> Result<String> {
    if let Some(url) = navigate {
        crate::commands::navigate::navigate(client, session_id, Some(url), false, false, false)
            .await?;
    }

    let eval = client
        .send_to_target(
            session_id,
            "Runtime.evaluate",
            json!({"expression": SNAPSHOT_JS, "returnByValue": true}),
        )
        .await?;
    let s: Value = eval["result"]["value"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!({}));

    if json_output {
        return Ok(serde_json::to_string_pretty(&s)?);
    }

    let ms = |v: &Value| match v.as_f64() {
        Some(n) if n > 0.0 => format!("{} ms", n.round() as i64),
        _ => "—".to_string(),
    };

    let mut out = String::from("Page timings:\n");
    out.push_str(&format!("  TTFB:                    {}\n", ms(&s["ttfb"])));
    out.push_str(&format!("  First Paint:             {}\n", ms(&s["firstPaint"])));
    out.push_str(&format!("  First Contentful Paint:  {}\n", ms(&s["firstContentfulPaint"])));
    out.push_str(&format!("  DOM Interactive:         {}\n", ms(&s["domInteractive"])));
    out.push_str(&format!("  DOMContentLoaded:        {}\n", ms(&s["domContentLoaded"])));
    out.push_str(&format!("  Load:                    {}\n", ms(&s["load"])));

    out.push_str("Page weight:\n");
    if let Some(n) = s["resources"].as_i64() {
        out.push_str(&format!("  Resources:               {n}\n"));
    }
    if let Some(n) = s["transferSize"].as_f64() {
        if n > 0.0 {
            out.push_str(&format!("  Transfer size:           {}\n", human_bytes(n)));
        }
    }
    if let Some(n) = s["domNodes"].as_i64() {
        out.push_str(&format!("  DOM nodes:               {n}\n"));
    }
    if let Some(v) = s["jsHeapUsed"].as_f64() {
        out.push_str(&format!("  JS heap used:            {}\n", human_bytes(v)));
    }
    if let Some(v) = s["jsHeapTotal"].as_f64() {
        out.push_str(&format!("  JS heap total:           {}\n", human_bytes(v)));
    }

    Ok(out.trim_end().to_string())
}

fn human_bytes(n: f64) -> String {
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
