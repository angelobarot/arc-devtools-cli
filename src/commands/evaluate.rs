use anyhow::Result;
use serde_json::json;

use crate::cdp::CdpClient;

pub async fn evaluate(
    client: &mut CdpClient,
    session_id: &str,
    expression: &str,
    as_json: bool,
) -> Result<String> {
    let result = client
        .send_to_target(
            session_id,
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
            }),
        )
        .await?;

    if let Some(exception) = result.get("exceptionDetails") {
        let text = exception["text"].as_str().unwrap_or("Unknown error");
        let desc = exception["exception"]["description"]
            .as_str()
            .unwrap_or(text);
        anyhow::bail!("{desc}");
    }

    let value = &result["result"];
    let val_type = value["type"].as_str().unwrap_or("undefined");

    if as_json {
        if let Some(v) = value.get("value") {
            Ok(serde_json::to_string_pretty(v)?)
        } else {
            Ok(serde_json::to_string_pretty(value)?)
        }
    } else {
        match val_type {
            "undefined" => Ok("undefined".to_string()),
            "string" => Ok(value["value"].as_str().unwrap_or("").to_string()),
            _ => {
                if let Some(v) = value.get("value") {
                    Ok(serde_json::to_string_pretty(v)?)
                } else {
                    Ok(value["description"].as_str().unwrap_or("").to_string())
                }
            }
        }
    }
}
