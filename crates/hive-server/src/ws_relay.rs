//! WebSocket relay between frontend clients and the room daemon.
//!
//! Each frontend WS connection gets paired with an upstream WS connection to
//! the room daemon. Messages flow bidirectionally:
//!
//! ```text
//! Frontend ←→ Hive WS Relay ←→ Room Daemon
//! ```
//!
//! Features:
//! - All message types forwarded (Text, Binary, Ping, Pong)
//! - Keepalive pings sent to daemon on configurable interval
//! - Automatic reconnection with exponential backoff on upstream failure
//! - 2-second connection timeout on upstream connects (local network)

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message as TungsteniteMsg};

use crate::daemon::{backoff_delay, DaemonWsConfig};
use crate::AppState;

/// Maximum reconnection attempts before giving up and closing the relay.
const MAX_RECONNECT_ATTEMPTS: u32 = 5;

/// Connection timeout for upstream daemon WebSocket connections.
///
/// Kept short (2s) since the relay connects to a co-located daemon on the
/// same host or local network. A longer timeout causes Playwright e2e tests
/// to hang when the daemon is unavailable (see #87).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Type alias for the daemon WebSocket stream.
type DaemonStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// GET /ws/:room_id — upgrade to WebSocket and relay to room daemon.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(room_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let daemon_ws_url = format!("{}/ws/{}", state.config.daemon.ws_url, room_id);
    let ws_config = DaemonWsConfig {
        ws_url: state.config.daemon.ws_url.clone(),
        ..DaemonWsConfig::default()
    };
    ws.on_upgrade(move |socket| relay(socket, daemon_ws_url, ws_config))
}

/// Connect to the daemon WebSocket with a timeout.
async fn connect_with_timeout(url: &str) -> Result<DaemonStream, String> {
    tokio::time::timeout(CONNECT_TIMEOUT, connect_async(url))
        .await
        .map_err(|_| format!("connection timed out after {CONNECT_TIMEOUT:?}"))?
        .map(|(ws, _)| ws)
        .map_err(|e| format!("connection failed: {e}"))
}

/// Convert a tungstenite message to an axum WebSocket message.
///
/// Returns `None` for Close and Frame messages, which are handled separately.
fn tungstenite_to_axum(msg: TungsteniteMsg) -> Option<Message> {
    match msg {
        TungsteniteMsg::Text(text) => Some(Message::Text(text.to_string().into())),
        TungsteniteMsg::Binary(data) => Some(Message::Binary(data.to_vec().into())),
        TungsteniteMsg::Ping(data) => Some(Message::Ping(data.to_vec().into())),
        TungsteniteMsg::Pong(data) => Some(Message::Pong(data.to_vec().into())),
        TungsteniteMsg::Close(_) => None,
        // Frame is an internal tungstenite type — not forwarded.
        _ => None,
    }
}

/// Convert an axum WebSocket message to a tungstenite message.
///
/// Returns `None` for Close messages, which are handled separately.
fn axum_to_tungstenite(msg: Message) -> Option<TungsteniteMsg> {
    match msg {
        Message::Text(text) => Some(TungsteniteMsg::Text(text.to_string().into())),
        Message::Binary(data) => Some(TungsteniteMsg::Binary(data.to_vec().into())),
        Message::Ping(data) => Some(TungsteniteMsg::Ping(data.to_vec().into())),
        Message::Pong(data) => Some(TungsteniteMsg::Pong(data.to_vec().into())),
        Message::Close(_) => None,
    }
}

/// Attempt to reconnect to the daemon with exponential backoff.
///
/// If `handshake` is provided, it is replayed after each successful connection
/// to re-authenticate with the daemon (the first frontend message is typically
/// a handshake frame).
///
/// Returns the reconnected stream on success, or `None` after exhausting attempts.
async fn try_reconnect(
    daemon_url: &str,
    config: &DaemonWsConfig,
    handshake: Option<&TungsteniteMsg>,
) -> Option<DaemonStream> {
    for attempt in 0..MAX_RECONNECT_ATTEMPTS {
        let delay = backoff_delay(attempt, config.max_backoff);
        tracing::info!(
            "reconnecting to daemon (attempt {}/{MAX_RECONNECT_ATTEMPTS}) after {delay:?}: {daemon_url}",
            attempt + 1,
        );
        tokio::time::sleep(delay).await;

        match connect_with_timeout(daemon_url).await {
            Ok(mut ws) => {
                if let Some(hs) = handshake {
                    if ws.send(hs.clone()).await.is_err() {
                        tracing::warn!("handshake replay failed on attempt {}", attempt + 1);
                        continue;
                    }
                }
                return Some(ws);
            }
            Err(e) => {
                tracing::warn!("reconnection attempt {} failed: {e}", attempt + 1);
            }
        }
    }
    tracing::error!("all {MAX_RECONNECT_ATTEMPTS} reconnection attempts failed: {daemon_url}");
    None
}

/// Bidirectional relay between a frontend WebSocket and a room daemon WebSocket.
///
/// Uses a single `select!` loop to handle:
/// - Frontend → daemon message forwarding (all types)
/// - Daemon → frontend message forwarding (all types)
/// - Periodic keepalive pings to the daemon
/// - Automatic reconnection when the daemon connection drops
async fn relay(frontend_ws: WebSocket, daemon_url: String, config: DaemonWsConfig) {
    // Connect upstream to room daemon with timeout.
    let upstream = match connect_with_timeout(&daemon_url).await {
        Ok(ws) => ws,
        Err(e) => {
            tracing::error!("failed to connect to room daemon at {daemon_url}: {e}");
            return;
        }
    };

    tracing::info!("relay established: frontend ↔ {daemon_url}");

    let (mut fe_tx, mut fe_rx) = frontend_ws.split();
    let (mut daemon_tx, mut daemon_rx) = upstream.split();

    let mut ping_interval = tokio::time::interval(config.ping_interval);
    // Skip the first immediate tick — first ping fires after one full interval.
    ping_interval.tick().await;

    // Cache the first frontend→daemon message for handshake replay on reconnect.
    let mut handshake: Option<TungsteniteMsg> = None;
    let mut is_first_message = true;

    loop {
        tokio::select! {
            // Frontend → daemon
            msg = fe_rx.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => {
                        let _ = daemon_tx.send(TungsteniteMsg::Close(None)).await;
                        break;
                    }
                    Some(Ok(fe_msg)) => {
                        if let Some(tung_msg) = axum_to_tungstenite(fe_msg) {
                            if is_first_message {
                                handshake = Some(tung_msg.clone());
                                is_first_message = false;
                            }
                            if daemon_tx.send(tung_msg).await.is_err() {
                                match try_reconnect(&daemon_url, &config, handshake.as_ref()).await {
                                    Some(ws) => {
                                        let (tx, rx) = ws.split();
                                        daemon_tx = tx;
                                        daemon_rx = rx;
                                        tracing::info!("relay reconnected: {daemon_url}");
                                    }
                                    None => break,
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        tracing::warn!("frontend receive error: {e}");
                        break;
                    }
                }
            }

            // Daemon → frontend
            msg = daemon_rx.next() => {
                match msg {
                    Some(Ok(TungsteniteMsg::Close(_))) | None => {
                        tracing::warn!("daemon disconnected: {daemon_url}");
                        match try_reconnect(&daemon_url, &config, handshake.as_ref()).await {
                            Some(ws) => {
                                let (tx, rx) = ws.split();
                                daemon_tx = tx;
                                daemon_rx = rx;
                                tracing::info!("relay reconnected: {daemon_url}");
                            }
                            None => {
                                let _ = fe_tx.send(Message::Close(None)).await;
                                break;
                            }
                        }
                    }
                    Some(Ok(daemon_msg)) => {
                        if let Some(axum_msg) = tungstenite_to_axum(daemon_msg) {
                            if fe_tx.send(axum_msg).await.is_err() {
                                tracing::info!("frontend gone, closing relay: {daemon_url}");
                                break;
                            }
                        }
                    }
                    Some(Err(e)) => {
                        tracing::warn!("daemon receive error: {e}");
                        match try_reconnect(&daemon_url, &config, handshake.as_ref()).await {
                            Some(ws) => {
                                let (tx, rx) = ws.split();
                                daemon_tx = tx;
                                daemon_rx = rx;
                                tracing::info!("relay reconnected after error: {daemon_url}");
                            }
                            None => {
                                let _ = fe_tx.send(Message::Close(None)).await;
                                break;
                            }
                        }
                    }
                }
            }

            // Keepalive ping to daemon
            _ = ping_interval.tick() => {
                if daemon_tx.send(TungsteniteMsg::Ping(Vec::new().into())).await.is_err() {
                    tracing::warn!("keepalive ping failed: {daemon_url}");
                    match try_reconnect(&daemon_url, &config, handshake.as_ref()).await {
                        Some(ws) => {
                            let (tx, rx) = ws.split();
                            daemon_tx = tx;
                            daemon_rx = rx;
                            tracing::info!("relay reconnected after ping failure: {daemon_url}");
                        }
                        None => break,
                    }
                }
            }
        }
    }

    tracing::info!("relay closed: {daemon_url}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tungstenite_text_converts_to_axum() {
        let msg = TungsteniteMsg::Text("hello".into());
        let result = tungstenite_to_axum(msg);
        assert!(result.is_some());
        if let Some(Message::Text(t)) = result {
            assert_eq!(t.to_string(), "hello");
        } else {
            panic!("expected Text variant");
        }
    }

    #[test]
    fn tungstenite_binary_converts_to_axum() {
        let data: Vec<u8> = vec![1, 2, 3];
        let msg = TungsteniteMsg::Binary(data.clone().into());
        let result = tungstenite_to_axum(msg);
        if let Some(Message::Binary(b)) = result {
            assert_eq!(b.to_vec(), data);
        } else {
            panic!("expected Binary variant");
        }
    }

    #[test]
    fn tungstenite_ping_converts_to_axum() {
        let data: Vec<u8> = vec![42];
        let msg = TungsteniteMsg::Ping(data.clone().into());
        let result = tungstenite_to_axum(msg);
        if let Some(Message::Ping(p)) = result {
            assert_eq!(p.to_vec(), data);
        } else {
            panic!("expected Ping variant");
        }
    }

    #[test]
    fn tungstenite_pong_converts_to_axum() {
        let data: Vec<u8> = vec![99];
        let msg = TungsteniteMsg::Pong(data.clone().into());
        let result = tungstenite_to_axum(msg);
        if let Some(Message::Pong(p)) = result {
            assert_eq!(p.to_vec(), data);
        } else {
            panic!("expected Pong variant");
        }
    }

    #[test]
    fn tungstenite_close_returns_none() {
        let msg = TungsteniteMsg::Close(None);
        assert!(tungstenite_to_axum(msg).is_none());
    }

    #[test]
    fn axum_text_converts_to_tungstenite() {
        let msg = Message::Text("world".into());
        let result = axum_to_tungstenite(msg);
        if let Some(TungsteniteMsg::Text(t)) = result {
            assert_eq!(t.to_string(), "world");
        } else {
            panic!("expected Text variant");
        }
    }

    #[test]
    fn axum_binary_converts_to_tungstenite() {
        let data: Vec<u8> = vec![4, 5, 6];
        let msg = Message::Binary(data.clone().into());
        let result = axum_to_tungstenite(msg);
        if let Some(TungsteniteMsg::Binary(b)) = result {
            assert_eq!(b.to_vec(), data);
        } else {
            panic!("expected Binary variant");
        }
    }

    #[test]
    fn axum_ping_converts_to_tungstenite() {
        let data: Vec<u8> = vec![7];
        let msg = Message::Ping(data.clone().into());
        let result = axum_to_tungstenite(msg);
        if let Some(TungsteniteMsg::Ping(p)) = result {
            assert_eq!(p.to_vec(), data);
        } else {
            panic!("expected Ping variant");
        }
    }

    #[test]
    fn axum_pong_converts_to_tungstenite() {
        let data: Vec<u8> = vec![8];
        let msg = Message::Pong(data.clone().into());
        let result = axum_to_tungstenite(msg);
        if let Some(TungsteniteMsg::Pong(p)) = result {
            assert_eq!(p.to_vec(), data);
        } else {
            panic!("expected Pong variant");
        }
    }

    #[test]
    fn axum_close_returns_none() {
        let msg = Message::Close(None);
        assert!(axum_to_tungstenite(msg).is_none());
    }

    #[test]
    fn text_roundtrip_preserves_content() {
        let original = "test message with unicode: 日本語";
        let tung = TungsteniteMsg::Text(original.into());
        let axum_msg = tungstenite_to_axum(tung).unwrap();
        let back = axum_to_tungstenite(axum_msg).unwrap();
        if let TungsteniteMsg::Text(t) = back {
            assert_eq!(t.to_string(), original);
        } else {
            panic!("expected Text variant");
        }
    }

    #[test]
    fn binary_roundtrip_preserves_content() {
        let data: Vec<u8> = vec![0, 1, 127, 128, 255];
        let tung = TungsteniteMsg::Binary(data.clone().into());
        let axum_msg = tungstenite_to_axum(tung).unwrap();
        let back = axum_to_tungstenite(axum_msg).unwrap();
        if let TungsteniteMsg::Binary(b) = back {
            assert_eq!(b.to_vec(), data);
        } else {
            panic!("expected Binary variant");
        }
    }

    #[test]
    fn empty_binary_roundtrip() {
        let tung = TungsteniteMsg::Binary(Vec::new().into());
        let axum_msg = tungstenite_to_axum(tung).unwrap();
        let back = axum_to_tungstenite(axum_msg).unwrap();
        if let TungsteniteMsg::Binary(b) = back {
            assert!(b.to_vec().is_empty());
        } else {
            panic!("expected Binary variant");
        }
    }

    #[test]
    fn connect_timeout_is_two_seconds() {
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(2));
    }

    #[test]
    fn max_reconnect_attempts_is_five() {
        assert_eq!(MAX_RECONNECT_ATTEMPTS, 5);
    }
}
