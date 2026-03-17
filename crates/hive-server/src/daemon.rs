//! WebSocket connection to the room daemon.
//!
//! Manages the upstream connection from hive-server to the co-located room
//! daemon. Handles connection, keepalive config, and exponential backoff
//! reconnection.

use std::time::Duration;

use futures_util::StreamExt;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

/// Configuration for connecting to the room daemon.
#[derive(Debug, Clone)]
pub struct DaemonWsConfig {
    /// Base WebSocket URL (e.g. `ws://127.0.0.1:4200`).
    pub ws_url: String,
    /// Keepalive ping interval.
    pub ping_interval: Duration,
    /// Maximum reconnection backoff.
    pub max_backoff: Duration,
}

impl Default for DaemonWsConfig {
    fn default() -> Self {
        Self {
            ws_url: "ws://127.0.0.1:4200".to_owned(),
            ping_interval: Duration::from_secs(30),
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// Connect to the room daemon's WebSocket endpoint for a specific room.
///
/// Returns the split (sink, stream) on success, or an error string.
pub async fn connect_to_room(
    config: &DaemonWsConfig,
    room_id: &str,
) -> Result<
    (
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            WsMessage,
        >,
        futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ),
    String,
> {
    let url = format!("{}/ws/{}", config.ws_url, room_id);
    tracing::info!("connecting to room daemon at {url}");

    let connect_timeout = Duration::from_secs(5);
    let (ws_stream, _) = tokio::time::timeout(connect_timeout, connect_async(&url))
        .await
        .map_err(|_| format!("connection to daemon timed out after {connect_timeout:?}"))?
        .map_err(|e| format!("failed to connect to daemon: {e}"))?;

    tracing::info!("connected to room daemon for room {room_id}");
    Ok(ws_stream.split())
}

/// Calculate exponential backoff delay for reconnection attempts.
pub fn backoff_delay(attempt: u32, max: Duration) -> Duration {
    let secs = 1u64 << attempt.min(63);
    Duration::from_secs(secs).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_increases_exponentially() {
        let max = Duration::from_secs(30);
        assert_eq!(backoff_delay(0, max), Duration::from_secs(1));
        assert_eq!(backoff_delay(1, max), Duration::from_secs(2));
        assert_eq!(backoff_delay(2, max), Duration::from_secs(4));
        assert_eq!(backoff_delay(3, max), Duration::from_secs(8));
        assert_eq!(backoff_delay(4, max), Duration::from_secs(16));
    }

    #[test]
    fn backoff_caps_at_max() {
        let max = Duration::from_secs(30);
        assert_eq!(backoff_delay(5, max), Duration::from_secs(30));
        assert_eq!(backoff_delay(10, max), Duration::from_secs(30));
    }

    #[test]
    fn default_config_values() {
        let config = DaemonWsConfig::default();
        assert_eq!(config.ws_url, "ws://127.0.0.1:4200");
        assert_eq!(config.ping_interval, Duration::from_secs(30));
        assert_eq!(config.max_backoff, Duration::from_secs(30));
    }
}
