use crate::friendly;
use anyhow::{anyhow, bail, Result};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

const RETRYABLE_CONNECTION_ERROR_PATTERNS: &[&str] = &[
    "websocket closed by server",
    "websocket connection closed",
    "websocket error:",
    "failed to connect to browser",
    "connection refused",
    "send after closing",
    "broken pipe",
    "connection reset",
    "already closed",
];

pub struct CdpClient {
    write: SplitSink<WsStream, Message>,
    read: SplitStream<WsStream>,
    next_id: u64,
}

#[derive(Debug, Clone)]
pub struct TargetInfo {
    pub target_id: String,
    pub title: String,
    pub url: String,
    #[allow(dead_code)]
    pub target_type: String,
}

impl CdpClient {
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (ws, _) = connect_async(ws_url)
            .await
            .map_err(|e| anyhow!("Failed to connect to browser at {ws_url}: {e}"))?;
        let (write, read) = ws.split();
        Ok(Self {
            write,
            read,
            next_id: 1,
        })
    }

    /// Send a browser-level CDP command.
    pub async fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        self.send_raw(method, params, None).await
    }

    /// Send a page-level CDP command (with session ID from attach_to_target).
    pub async fn send_to_target(
        &mut self,
        session_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.send_raw(method, params, Some(session_id)).await
    }

    async fn send_raw(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let mut msg = json!({"id": id, "method": method});
        if !params.is_null() && params != json!({}) {
            msg["params"] = params;
        }
        if let Some(sid) = session_id {
            msg["sessionId"] = json!(sid);
        }

        let text = serde_json::to_string(&msg)?;
        self.write.send(Message::Text(text)).await?;

        loop {
            let resp_text = self.read_text().await?;
            let resp: Value = serde_json::from_str(&resp_text)?;

            if resp.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(error) = resp.get("error") {
                    bail!(
                        "CDP error in {method}: {}",
                        serde_json::to_string_pretty(error)?
                    );
                }
                return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
            }
            // Skip events and unrelated responses
        }
    }

    /// Read until we get an event with the given method name (for waiting on page load, etc).
    #[allow(dead_code)]
    pub async fn wait_for_event(
        &mut self,
        event_method: &str,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                bail!("Timeout waiting for event {event_method}");
            }
            let text = tokio::time::timeout(remaining, self.read_text())
                .await
                .map_err(|_| anyhow!("Timeout waiting for event {event_method}"))??;
            let resp: Value = serde_json::from_str(&text)?;
            if resp.get("method").and_then(|v| v.as_str()) == Some(event_method) {
                return Ok(resp.get("params").cloned().unwrap_or(Value::Null));
            }
        }
    }

    /// Send a CDP message without waiting for a response.
    /// Used for fire-and-forget acks (e.g. Page.screencastFrameAck).
    pub async fn send_fire_and_forget(
        &mut self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<()> {
        let id = self.next_id;
        self.next_id += 1;

        let mut msg = json!({"id": id, "method": method});
        if !params.is_null() && params != json!({}) {
            msg["params"] = params;
        }
        if let Some(sid) = session_id {
            msg["sessionId"] = json!(sid);
        }

        let text = serde_json::to_string(&msg)?;
        self.write.send(Message::Text(text)).await?;
        Ok(())
    }

    /// Read the next WebSocket message as parsed JSON.
    /// Returns None on timeout, Some(value) on message, or Err on connection failure.
    pub async fn read_next_message(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<Value>> {
        match tokio::time::timeout(timeout, self.read_text()).await {
            Ok(Ok(text)) => {
                let value: Value = serde_json::from_str(&text)?;
                Ok(Some(value))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(None), // timeout
        }
    }

    async fn read_text(&mut self) -> Result<String> {
        loop {
            match self.read.next().await {
                Some(Ok(Message::Text(text))) => return Ok(text.to_string()),
                Some(Ok(Message::Close(_))) => bail!("WebSocket closed by server"),
                Some(Ok(_)) => continue,
                Some(Err(e)) => bail!("WebSocket error: {e}"),
                None => bail!("WebSocket connection closed"),
            }
        }
    }

    // ── Target domain helpers ──

    pub async fn get_page_targets(&mut self) -> Result<Vec<TargetInfo>> {
        let result = self.send("Target.getTargets", json!({})).await?;
        let targets = result["targetInfos"]
            .as_array()
            .ok_or_else(|| anyhow!("Unexpected getTargets response"))?;

        let mut pages = Vec::new();
        for t in targets {
            let target_type = t["type"].as_str().unwrap_or("");
            if target_type == "page" {
                pages.push(TargetInfo {
                    target_id: t["targetId"].as_str().unwrap_or("").to_string(),
                    title: t["title"].as_str().unwrap_or("").to_string(),
                    url: t["url"].as_str().unwrap_or("").to_string(),
                    target_type: target_type.to_string(),
                });
            }
        }
        Ok(pages)
    }

    pub async fn attach_to_target(&mut self, target_id: &str) -> Result<String> {
        let result = self
            .send(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
            )
            .await?;
        result["sessionId"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("No sessionId in attachToTarget response"))
    }

    pub async fn detach_from_target(&mut self, session_id: &str) -> Result<()> {
        self.send("Target.detachFromTarget", json!({"sessionId": session_id}))
            .await?;
        Ok(())
    }

    pub async fn activate_target(&mut self, target_id: &str) -> Result<()> {
        self.send("Target.activateTarget", json!({"targetId": target_id}))
            .await?;
        Ok(())
    }

    pub async fn create_target(&mut self, url: &str) -> Result<String> {
        let result = self
            .send("Target.createTarget", json!({"url": url}))
            .await?;
        result["targetId"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("No targetId in createTarget response"))
    }

    pub async fn close_target(&mut self, target_id: &str) -> Result<()> {
        self.send("Target.closeTarget", json!({"targetId": target_id}))
            .await?;
        Ok(())
    }

    /// Resolve which page to operate on.
    /// Priority: --target (by ID or friendly name) > --page (by index) > first page.
    pub async fn resolve_page(
        &mut self,
        target: Option<&str>,
        page: Option<usize>,
    ) -> Result<TargetInfo> {
        let pages = self.get_page_targets().await?;

        if let Some(tid) = target {
            if friendly::is_friendly(tid) {
                // Resolve friendly name → target ID
                return pages
                    .into_iter()
                    .find(|p| friendly::to_friendly(&p.target_id) == tid)
                    .ok_or_else(|| anyhow!("No page matching '{tid}'"));
            }
            // Raw target ID
            return pages
                .into_iter()
                .find(|p| p.target_id == tid)
                .ok_or_else(|| anyhow!("No page with target ID: {tid}"));
        }

        let idx = page.unwrap_or(0);
        pages
            .into_iter()
            .nth(idx)
            .ok_or_else(|| anyhow!("No page at index {idx}"))
    }
}

pub fn is_retryable_connection_error_message(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    RETRYABLE_CONNECTION_ERROR_PATTERNS
        .iter()
        .any(|pattern| normalized.contains(pattern))
}

pub fn is_retryable_connection_error(error: &anyhow::Error) -> bool {
    is_retryable_connection_error_message(&format!("{error:#}"))
}

/// Compute the expected frame interval for a target FPS.
pub fn frame_interval_ms(fps: u32) -> u64 {
    if fps == 0 {
        return 1000;
    }
    1000 / fps as u64
}

/// Check whether enough time has elapsed since `last_ms` to emit the next frame
/// at the given FPS. Returns the updated timestamp if a frame should be emitted.
pub fn should_emit_frame(last_ms: u64, now_ms: u64, fps: u32) -> Option<u64> {
    let interval = frame_interval_ms(fps);
    if now_ms >= last_ms + interval {
        Some(now_ms)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::is_retryable_connection_error_message;

    #[test]
    fn detects_retryable_websocket_failures() {
        assert!(is_retryable_connection_error_message(
            "WebSocket closed by server"
        ));
        assert!(is_retryable_connection_error_message(
            "WebSocket error: Connection reset without closing handshake"
        ));
        assert!(is_retryable_connection_error_message(
            "Failed to connect to browser at ws://127.0.0.1:9222/devtools/browser/abc"
        ));
        assert!(is_retryable_connection_error_message(
            "Broken pipe while writing request"
        ));
    }

    #[test]
    fn ignores_non_retryable_page_selection_failures() {
        assert!(!is_retryable_connection_error_message(
            "No page matching 'red-snake'"
        ));
        assert!(!is_retryable_connection_error_message("No page at index 3"));
    }

    use super::{frame_interval_ms, should_emit_frame};

    #[test]
    fn frame_interval_standard_fps() {
        assert_eq!(frame_interval_ms(12), 83); // 1000/12 = 83
        assert_eq!(frame_interval_ms(24), 41); // 1000/24 = 41
        assert_eq!(frame_interval_ms(30), 33); // 1000/30 = 33
        assert_eq!(frame_interval_ms(1), 1000);
    }

    #[test]
    fn frame_interval_zero_fps_defaults() {
        assert_eq!(frame_interval_ms(0), 1000);
    }

    #[test]
    fn should_emit_frame_timing() {
        // At 12 fps, interval is 83ms
        assert!(should_emit_frame(0, 83, 12).is_some());
        assert!(should_emit_frame(0, 82, 12).is_none());
        assert!(should_emit_frame(0, 100, 12).is_some());
        // After emitting at t=83, next frame at t=166
        assert!(should_emit_frame(83, 165, 12).is_none());
        assert!(should_emit_frame(83, 166, 12).is_some());
    }

    #[test]
    fn should_emit_frame_same_timestamp() {
        // Same timestamp as last emit — should not emit again
        assert!(should_emit_frame(100, 100, 12).is_none());
    }
}
