use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const DAEMON_IDLE_TIMEOUT_ENV: &str = "ARC_DEVTOOLS_DAEMON_IDLE_TIMEOUT";

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonIdleTimeout {
    Seconds(u64),
    Never,
}

impl DaemonIdleTimeout {
    pub const DEFAULT: Self = Self::Seconds(300);

    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        if normalized == "never" {
            return Ok(Self::Never);
        }

        let unit_start = normalized
            .find(|ch: char| !ch.is_ascii_digit())
            .ok_or_else(|| anyhow::anyhow!("expected a unit suffix: s, m, h, or 'never'"))?;

        let (amount_raw, unit) = normalized.split_at(unit_start);
        if amount_raw.is_empty() || unit.is_empty() {
            anyhow::bail!(
                "invalid timeout '{value}', expected format like 30m, 1h, 300s, or never"
            );
        }

        let amount: u64 = amount_raw
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid timeout number '{amount_raw}'"))?;
        if amount == 0 {
            anyhow::bail!("idle timeout must be greater than zero, or use 'never'");
        }

        let seconds = match unit {
            "s" => amount,
            "m" => amount
                .checked_mul(60)
                .ok_or_else(|| anyhow::anyhow!("timeout too large"))?,
            "h" => amount
                .checked_mul(60 * 60)
                .ok_or_else(|| anyhow::anyhow!("timeout too large"))?,
            _ => anyhow::bail!("invalid timeout unit '{unit}', expected s, m, h, or 'never'"),
        };

        Ok(Self::Seconds(seconds))
    }

    pub fn as_duration(self) -> Option<Duration> {
        match self {
            Self::Seconds(seconds) => Some(Duration::from_secs(seconds)),
            Self::Never => None,
        }
    }
}

impl std::fmt::Display for DaemonIdleTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Never => write!(f, "never"),
            Self::Seconds(seconds) if seconds % 3600 == 0 => write!(f, "{}h", seconds / 3600),
            Self::Seconds(seconds) if seconds % 60 == 0 => write!(f, "{}m", seconds / 60),
            Self::Seconds(seconds) => write!(f, "{}s", seconds),
        }
    }
}

/// Request from CLI client to daemon.
#[derive(Serialize, Deserialize, Debug)]
pub struct DaemonRequest {
    pub command: String,
    pub args: Value,
    pub page: Option<usize>,
    pub target: Option<String>,
    pub json_output: bool,
    #[serde(default)]
    pub daemon_idle_timeout: Option<DaemonIdleTimeout>,
    #[serde(default)]
    pub ws_endpoint: Option<String>,
    #[serde(default)]
    pub user_data_dir: Option<String>,
}

/// Response from daemon to CLI client.
#[derive(Serialize, Deserialize, Debug)]
pub struct DaemonResponse {
    pub success: bool,
    pub output: String,
    pub error: String,
}

pub fn socket_path() -> PathBuf {
    std::env::temp_dir().join("arc-devtools-daemon.sock")
}

pub fn pid_path() -> PathBuf {
    std::env::temp_dir().join("arc-devtools-daemon.pid")
}

pub fn log_path() -> PathBuf {
    std::env::temp_dir().join("arc-devtools-daemon.log")
}

/// Write a length-prefixed message to a stream.
pub async fn write_msg<W: AsyncWriteExt + Unpin>(w: &mut W, data: &[u8]) -> anyhow::Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    w.write_all(&len).await?;
    w.write_all(data).await?;
    w.flush().await?;
    Ok(())
}

/// Read a length-prefixed message from a stream.
pub async fn read_msg<R: AsyncReadExt + Unpin>(r: &mut R) -> anyhow::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 64 * 1024 * 1024 {
        anyhow::bail!("Message too large: {len} bytes");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::DaemonIdleTimeout;
    use std::time::Duration;

    #[test]
    fn parses_supported_timeout_values() {
        assert_eq!(
            DaemonIdleTimeout::parse("30m").unwrap(),
            DaemonIdleTimeout::Seconds(1800)
        );
        assert_eq!(
            DaemonIdleTimeout::parse("1h").unwrap(),
            DaemonIdleTimeout::Seconds(3600)
        );
        assert_eq!(
            DaemonIdleTimeout::parse("300s").unwrap(),
            DaemonIdleTimeout::Seconds(300)
        );
        assert_eq!(
            DaemonIdleTimeout::parse("never").unwrap(),
            DaemonIdleTimeout::Never
        );
    }

    #[test]
    fn rejects_invalid_timeout_values() {
        assert!(DaemonIdleTimeout::parse("0m").is_err());
        assert!(DaemonIdleTimeout::parse("30").is_err());
        assert!(DaemonIdleTimeout::parse("15d").is_err());
    }

    #[test]
    fn converts_to_duration_when_finite() {
        assert_eq!(
            DaemonIdleTimeout::Seconds(90).as_duration(),
            Some(Duration::from_secs(90))
        );
        assert_eq!(DaemonIdleTimeout::Never.as_duration(), None);
    }
}
