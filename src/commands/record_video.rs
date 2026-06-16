use anyhow::{anyhow, bail, Result};
use base64::Engine;
use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::cdp::{self, CdpClient};

/// Parameters for the record-video command.
pub struct RecordVideoParams {
    pub output: String,
    pub duration_secs: u64,
    pub fps: u32,
    pub quality: u32,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
}

/// Record the current page to an MP4 file using CDP screencast + ffmpeg.
pub async fn record_video(
    client: &mut CdpClient,
    session_id: &str,
    params: &RecordVideoParams,
) -> Result<String> {
    validate_record_video_params(params)?;

    // Fail fast if ffmpeg is not available
    let ffmpeg_path = find_ffmpeg()?;

    // Build screencast params
    let mut screencast_params = json!({
        "format": "jpeg",
        "quality": params.quality,
        "everyNthFrame": 1,
    });
    if let Some(w) = params.max_width {
        screencast_params["maxWidth"] = json!(w);
    }
    if let Some(h) = params.max_height {
        screencast_params["maxHeight"] = json!(h);
    }

    // Start screencast
    client
        .send_to_target(session_id, "Page.startScreencast", screencast_params)
        .await?;

    // Spawn ffmpeg after screencast starts so failures do not leak a child process.
    let mut ffmpeg = match spawn_ffmpeg(&ffmpeg_path, params) {
        Ok(child) => child,
        Err(error) => {
            let _ = client
                .send_to_target(session_id, "Page.stopScreencast", json!({}))
                .await;
            return Err(error);
        }
    };

    let mut stdin = ffmpeg
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Failed to open ffmpeg stdin"))?;

    let start = Instant::now();
    let duration = std::time::Duration::from_secs(params.duration_secs);
    let frame_interval = cdp::frame_interval_ms(params.fps);
    let mut last_emit_ms: Option<u64> = None;
    let mut frames_written: u64 = 0;
    let mut capture_error = None;

    // Read screencast frames for the specified duration
    loop {
        let elapsed = start.elapsed();
        if elapsed >= duration {
            break;
        }

        let remaining = duration - elapsed;
        // Use a short read timeout — we poll frequently to check elapsed time
        let read_timeout = remaining.min(std::time::Duration::from_millis(frame_interval));

        let msg = match client.read_next_message(read_timeout).await {
            Ok(Some(msg)) => msg,
            Ok(None) => continue, // timeout, loop to check elapsed
            Err(e) => {
                capture_error = Some(format!("WebSocket error during recording: {e}"));
                break;
            }
        };

        // Check if this is a screencastFrame event
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        if method != "Page.screencastFrame" {
            continue;
        }

        let event_params = match msg.get("params") {
            Some(p) => p,
            None => continue,
        };

        // Ack the frame promptly (fire-and-forget)
        let ack_session_id = event_params
            .get("sessionId")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let _ = client
            .send_fire_and_forget(
                "Page.screencastFrameAck",
                json!({"sessionId": ack_session_id}),
                Some(session_id),
            )
            .await;

        // Rate-limit frame emission to target FPS using local elapsed timing
        let now_ms = start.elapsed().as_millis() as u64;
        if let Some(last_ms) = last_emit_ms {
            if cdp::should_emit_frame(last_ms, now_ms, params.fps).is_none() {
                continue;
            }
        }
        last_emit_ms = Some(now_ms);

        // Decode base64 JPEG data and write to ffmpeg stdin
        let data_b64 = match event_params.get("data").and_then(|v| v.as_str()) {
            Some(d) => d,
            None => continue,
        };

        let jpeg_bytes = match base64::engine::general_purpose::STANDARD.decode(data_b64) {
            Ok(b) => b,
            Err(_) => continue,
        };

        if let Err(e) = stdin.write_all(&jpeg_bytes) {
            capture_error = Some(format!("Failed to write frame to ffmpeg: {e}"));
            break;
        }

        frames_written += 1;
    }

    // Stop screencast (best-effort)
    let _ = client
        .send_to_target(session_id, "Page.stopScreencast", json!({}))
        .await;

    // Close ffmpeg stdin to signal end of input
    drop(stdin);

    if frames_written == 0 {
        let _ = ffmpeg.kill();
        let _ = ffmpeg.wait();

        if let Some(error) = capture_error {
            bail!("{error}");
        }

        bail!(
            "No frames captured during recording — the page may not be visible or screencast may not be supported"
        );
    }

    // Wait for ffmpeg to finish
    let ffmpeg_output = ffmpeg
        .wait_with_output()
        .map_err(|e| anyhow!("Failed to wait for ffmpeg: {e}"))?;

    if !ffmpeg_output.status.success() {
        let stderr = String::from_utf8_lossy(&ffmpeg_output.stderr);
        bail!(
            "ffmpeg exited with status {}: {}",
            ffmpeg_output.status,
            stderr.lines().last().unwrap_or("unknown error")
        );
    }

    if let Some(err) = capture_error {
        bail!("{err}");
    }

    let file_size = std::fs::metadata(&params.output)
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(format!(
        "Video saved to {} ({} frames, {:.1}s, {} bytes)",
        params.output, frames_written, params.duration_secs, file_size
    ))
}

fn validate_record_video_params(params: &RecordVideoParams) -> Result<()> {
    if params.duration_secs == 0 {
        bail!("duration must be greater than 0 seconds");
    }

    if !(1..=1000).contains(&params.fps) {
        bail!("fps must be between 1 and 1000");
    }

    if !(1..=100).contains(&params.quality) {
        bail!("quality must be between 1 and 100");
    }

    Ok(())
}

fn spawn_ffmpeg(ffmpeg_path: &str, params: &RecordVideoParams) -> Result<std::process::Child> {
    Command::new(ffmpeg_path)
        .args([
            "-y", // overwrite output
            "-f",
            "image2pipe", // input is piped images
            "-framerate",
            &params.fps.to_string(),
            "-vcodec",
            "mjpeg", // input codec
            "-i",
            "-", // read from stdin
            "-c:v",
            "libx264", // output codec
            "-pix_fmt",
            "yuv420p", // compatibility
            "-preset",
            "fast",
            "-movflags",
            "+faststart", // web-friendly MP4
            &params.output,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| anyhow!("Failed to start ffmpeg: {error}"))
}

/// Find ffmpeg in PATH, returning the path or a clear error.
fn find_ffmpeg() -> Result<String> {
    match which_command("ffmpeg") {
        Some(path) => Ok(path),
        None => bail!(
            "ffmpeg not found in PATH. Install it to use record-video:\n  \
             macOS:  brew install ffmpeg\n  \
             Ubuntu: sudo apt install ffmpeg\n  \
             Other:  https://ffmpeg.org/download.html"
        ),
    }
}

/// Portable which(1) — check if a command exists in PATH.
fn which_command(cmd: &str) -> Option<String> {
    let check = if cfg!(target_os = "windows") {
        Command::new("where").arg(cmd).output()
    } else {
        Command::new("which").arg(cmd).output()
    };

    match check {
        Ok(output) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                None
            } else {
                // Take first line in case of multiple results
                Some(path.lines().next().unwrap_or(&path).to_string())
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_ffmpeg_returns_path_or_clear_error() {
        // This test validates the error message format when ffmpeg is missing.
        // On CI without ffmpeg, it should produce a helpful message.
        // On dev machines with ffmpeg, it should return a valid path.
        match find_ffmpeg() {
            Ok(path) => assert!(!path.is_empty()),
            Err(e) => {
                let msg = format!("{e}");
                assert!(msg.contains("ffmpeg not found"));
                assert!(msg.contains("brew install ffmpeg"));
            }
        }
    }

    #[test]
    fn which_command_finds_common_binary() {
        // `ls` should exist on all Unix systems
        if cfg!(not(target_os = "windows")) {
            assert!(which_command("ls").is_some());
        }
    }

    #[test]
    fn which_command_returns_none_for_missing() {
        assert!(which_command("this_binary_definitely_does_not_exist_xyz").is_none());
    }

    #[test]
    fn record_video_params_defaults() {
        let params = RecordVideoParams {
            output: "/tmp/test.mp4".to_string(),
            duration_secs: 5,
            fps: 12,
            quality: 80,
            max_width: None,
            max_height: None,
        };
        assert_eq!(params.duration_secs, 5);
        assert_eq!(params.fps, 12);
        assert_eq!(params.quality, 80);
        assert!(params.max_width.is_none());
    }

    #[test]
    fn reject_zero_fps() {
        let params = RecordVideoParams {
            output: "/tmp/test.mp4".to_string(),
            duration_secs: 5,
            fps: 0,
            quality: 80,
            max_width: None,
            max_height: None,
        };

        let error = validate_record_video_params(&params).unwrap_err();
        assert!(format!("{error}").contains("fps must be between 1 and 1000"));
    }

    #[test]
    fn reject_over_max_fps() {
        let params = RecordVideoParams {
            output: "/tmp/test.mp4".to_string(),
            duration_secs: 5,
            fps: 1001,
            quality: 80,
            max_width: None,
            max_height: None,
        };

        let error = validate_record_video_params(&params).unwrap_err();
        assert!(format!("{error}").contains("fps must be between 1 and 1000"));
    }

    #[test]
    fn reject_out_of_range_quality() {
        let params = RecordVideoParams {
            output: "/tmp/test.mp4".to_string(),
            duration_secs: 5,
            fps: 12,
            quality: 101,
            max_width: None,
            max_height: None,
        };

        let error = validate_record_video_params(&params).unwrap_err();
        assert!(format!("{error}").contains("quality must be between 1 and 100"));
    }
}
