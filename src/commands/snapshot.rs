use anyhow::Result;
use serde_json::json;
use std::fmt::Write;

use crate::cdp::CdpClient;

pub async fn take_snapshot(
    client: &mut CdpClient,
    session_id: &str,
    as_json: bool,
) -> Result<String> {
    let result = client
        .send_to_target(session_id, "Accessibility.getFullAXTree", json!({}))
        .await?;

    if as_json {
        return Ok(serde_json::to_string_pretty(&result)?);
    }

    let nodes = result["nodes"].as_array();
    if let Some(nodes) = nodes {
        let mut out = String::new();
        for node in nodes {
            let role = node["role"]["value"].as_str().unwrap_or("");
            let name = node["name"]["value"].as_str().unwrap_or("");
            let node_id = node["nodeId"].as_str().unwrap_or("");

            if role == "none" || role == "generic" || role == "Ignored" {
                continue;
            }

            let depth = node["depth"].as_u64().unwrap_or(0) as usize;
            let indent = "  ".repeat(depth);

            if name.is_empty() {
                writeln!(out, "{indent}[{role}] #{node_id}").unwrap();
            } else {
                writeln!(out, "{indent}[{role}] \"{name}\" #{node_id}").unwrap();
            }
        }
        Ok(out)
    } else {
        Ok(serde_json::to_string_pretty(&result)?)
    }
}
