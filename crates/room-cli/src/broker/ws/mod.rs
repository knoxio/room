use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket},
        Path, State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{broadcast, Mutex};

use super::{
    auth::{check_join_permission, issue_token, validate_token},
    state::RoomState,
};

pub(crate) mod rest;

/// Shared state for axum handlers.
#[derive(Clone)]
pub(crate) struct WsAppState {
    pub(crate) room_state: Arc<RoomState>,
    pub(crate) next_client_id: Arc<AtomicU64>,
    /// Global user registry for daemon mode. `None` in single-room mode.
    pub(crate) user_registry: Option<Arc<Mutex<crate::registry::UserRegistry>>>,
}

/// Build the axum router with WebSocket and REST routes.
pub(crate) fn create_router(state: WsAppState) -> Router {
    Router::new()
        .route("/ws/{room_id}", get(ws_upgrade_handler))
        .route("/api/{room_id}/join", post(rest::api_join))
        .route("/api/{room_id}/send", post(rest::api_send))
        .route("/api/{room_id}/poll", get(rest::api_poll))
        .route("/api/{room_id}/query", get(rest::api_query))
        .route("/api/health", get(rest::api_health))
        .with_state(state)
}

// ── WebSocket upgrade ───────────────────────────────────────────────────

async fn ws_upgrade_handler(
    ws: WebSocketUpgrade,
    Path(room_id): Path<String>,
    State(state): State<WsAppState>,
) -> impl IntoResponse {
    if room_id != *state.room_state.room_id {
        return (StatusCode::NOT_FOUND, "room not found").into_response();
    }
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = handle_ws_client(socket, state).await {
            eprintln!("[broker/ws] error: {e:#}");
        }
    })
}

// ── WebSocket client lifecycle ──────────────────────────────────────────

async fn handle_ws_client(ws: WebSocket, app_state: WsAppState) -> anyhow::Result<()> {
    let state = app_state.room_state.clone();
    let registry = app_state.user_registry.clone();
    let cid = app_state.next_client_id.fetch_add(1, Ordering::SeqCst) + 1;

    let (tx, _) = broadcast::channel::<String>(256);
    state
        .clients
        .lock()
        .await
        .insert(cid, (String::new(), tx.clone()));

    let result = run_ws_session(cid, ws, tx, &state, registry.as_ref()).await;
    state.clients.lock().await.remove(&cid);
    result
}

async fn run_ws_session(
    cid: u64,
    ws: WebSocket,
    own_tx: broadcast::Sender<String>,
    state: &Arc<RoomState>,
    user_registry: Option<&Arc<Mutex<crate::registry::UserRegistry>>>,
) -> anyhow::Result<()> {
    let (mut ws_tx, mut ws_rx) = ws.split();

    // Read first frame: handshake (same protocol as UDS).
    let first_frame = match ws_rx.next().await {
        Some(Ok(WsMessage::Text(text))) => text.trim().to_owned(),
        Some(Ok(WsMessage::Close(_))) | None => return Ok(()),
        Some(Ok(_)) => return Ok(()),
        Some(Err(e)) => return Err(e.into()),
    };

    use super::handshake::{parse_client_handshake, ClientHandshake};
    let username = match parse_client_handshake(&first_frame) {
        ClientHandshake::Join(u) => {
            return ws_oneshot_join(&u, &mut ws_tx, state).await;
        }
        ClientHandshake::Send(u) => {
            eprintln!(
                "[broker/ws] DEPRECATED: SEND:{u} handshake used — \
                 migrate to TOKEN:<uuid> (SEND: will be removed in a future version)"
            );
            return ws_oneshot_send(u, &mut ws_rx, &mut ws_tx, state).await;
        }
        ClientHandshake::Token(token) => {
            // Try room-level token map first, then fall back to global UserRegistry.
            let resolved = match validate_token(&token, &state.auth.token_map).await {
                Some(u) => Some(u),
                None => {
                    if let Some(reg) = user_registry {
                        reg.lock()
                            .await
                            .validate_token(&token)
                            .map(|u| u.to_owned())
                    } else {
                        None
                    }
                }
            };
            return match resolved {
                Some(u) => ws_oneshot_send(u, &mut ws_rx, &mut ws_tx, state).await,
                None => {
                    let err = serde_json::json!({"type":"error","code":"invalid_token"});
                    let _ = ws_tx.send(WsMessage::Text(err.to_string().into())).await;
                    Ok(())
                }
            };
        }
        ClientHandshake::Session(token) => {
            // Resolve username from token (room-level first, then UserRegistry).
            let resolved = match validate_token(&token, &state.auth.token_map).await {
                Some(u) => Some(u),
                None => {
                    if let Some(reg) = user_registry {
                        reg.lock()
                            .await
                            .validate_token(&token)
                            .map(|u| u.to_owned())
                    } else {
                        None
                    }
                }
            };
            match resolved {
                Some(u) => u,
                None => {
                    let err = serde_json::json!({"type":"error","code":"invalid_token"});
                    let _ = ws_tx.send(WsMessage::Text(err.to_string().into())).await;
                    return Ok(());
                }
            }
        }
        ClientHandshake::Interactive(u) => {
            eprintln!(
                "[broker/ws] DEPRECATED: unauthenticated interactive join for '{u}' — \
                 migrate to SESSION:<token>"
            );
            u
        }
    };

    // Interactive join (authenticated via SESSION: or deprecated plain username).
    if username.is_empty() {
        return Ok(());
    }

    // Check join permission before entering interactive session.
    if let Err(reason) = check_join_permission(&username, state.config.as_ref()) {
        let err = serde_json::json!({
            "type": "error",
            "code": "join_denied",
            "message": reason,
            "username": username
        });
        let _ = ws_tx.send(WsMessage::Text(err.to_string().into())).await;
        return Ok(());
    }

    // Subscribe before setup so we don't miss concurrent messages.
    let mut rx = own_tx.subscribe();

    // Shared setup: register client, elect host, load history, broadcast join.
    let history_lines = match super::session::session_setup(cid, &username, state).await {
        Ok(lines) => lines,
        Err(e) => {
            eprintln!("[broker/ws] session_setup failed: {e:#}");
            return Ok(());
        }
    };

    // Send history as WS frames.
    for line in &history_lines {
        if ws_tx
            .send(WsMessage::Text(line.clone().into()))
            .await
            .is_err()
        {
            return Ok(());
        }
    }

    // Wrap sender for shared use by outbound and shutdown paths.
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    // Outbound: forward broadcast channel messages to the WebSocket.
    let ws_tx_out = ws_tx.clone();
    let mut shutdown_rx = state.shutdown.subscribe();
    let outbound = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(line) => {
                            let trimmed = line.trim_end().to_owned();
                            if ws_tx_out.lock().await.send(WsMessage::Text(trimmed.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("[broker/ws] cid={cid} lagged by {n}");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = shutdown_rx.changed() => {
                    while let Ok(line) = rx.try_recv() {
                        let trimmed = line.trim_end().to_owned();
                        let _ = ws_tx_out.lock().await.send(WsMessage::Text(trimmed.into())).await;
                    }
                    let _ = ws_tx_out.lock().await.send(WsMessage::Close(None)).await;
                    break;
                }
            }
        }
    });

    // Inbound: read text frames, delegate to shared processing.
    let username_in = username.clone();
    let ws_tx_in = ws_tx.clone();
    let state_in = state.clone();
    let inbound = tokio::spawn(async move {
        while let Some(frame) = ws_rx.next().await {
            match frame {
                Ok(WsMessage::Text(text)) => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match super::session::process_inbound_message(trimmed, &username_in, &state_in)
                        .await
                    {
                        super::session::InboundResult::Ok => {}
                        super::session::InboundResult::Reply(json) => {
                            let _ = ws_tx_in
                                .lock()
                                .await
                                .send(WsMessage::Text(json.into()))
                                .await;
                        }
                        super::session::InboundResult::Shutdown => break,
                    }
                }
                Ok(WsMessage::Close(_)) => break,
                Ok(_) => {} // ping/pong handled automatically, binary ignored
                Err(_) => break,
            }
        }
    });

    tokio::select! {
        _ = outbound => {},
        _ = inbound => {},
    }

    // Shared teardown: remove status, broadcast leave.
    super::session::session_teardown(cid, &username, state).await;

    Ok(())
}

// ── WebSocket one-shot handlers ─────────────────────────────────────────

type WsSink = futures_util::stream::SplitSink<WebSocket, WsMessage>;
type WsStream = futures_util::stream::SplitStream<WebSocket>;

async fn ws_oneshot_join(
    username: &str,
    ws_tx: &mut WsSink,
    state: &Arc<RoomState>,
) -> anyhow::Result<()> {
    // Check room visibility/ACL before issuing a token.
    if let Err(reason) = super::auth::check_join_permission(username, state.config.as_ref()) {
        let err = serde_json::json!({
            "type": "error",
            "code": "join_denied",
            "message": reason,
            "username": username
        });
        let _ = ws_tx.send(WsMessage::Text(err.to_string().into())).await;
        let _ = ws_tx.send(WsMessage::Close(None)).await;
        return Ok(());
    }
    match issue_token(
        username,
        &state.auth.token_map,
        Some(&state.auth.token_map_path),
    )
    .await
    {
        Ok(token) => {
            let resp = serde_json::json!({"type":"token","token": token, "username": username});
            let _ = ws_tx.send(WsMessage::Text(resp.to_string().into())).await;
            // Set Full subscription so the joining user receives all messages,
            // matching the UDS join behaviour in auth.rs.
            state
                .filters
                .subscription_map
                .lock()
                .await
                .insert(username.to_owned(), room_protocol::SubscriptionTier::Full);
            // Persist so the subscription survives broker restart.
            super::persistence::persist_subscriptions(state).await;
        }
        Err(_) => {
            let err = serde_json::json!({
                "type":"error","code":"username_taken","username": username
            });
            let _ = ws_tx.send(WsMessage::Text(err.to_string().into())).await;
        }
    }
    let _ = ws_tx.send(WsMessage::Close(None)).await;
    Ok(())
}

async fn ws_oneshot_send(
    username: String,
    ws_rx: &mut WsStream,
    ws_tx: &mut WsSink,
    state: &Arc<RoomState>,
) -> anyhow::Result<()> {
    // Read the message frame.
    let text = match ws_rx.next().await {
        Some(Ok(WsMessage::Text(t))) => t,
        _ => return Ok(()),
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let super::session::OneshotResult::Reply(reply) =
        super::session::process_oneshot_send(trimmed, &username, state).await?;
    let _ = ws_tx.send(WsMessage::Text(reply.into())).await;
    Ok(())
}

// ── Daemon-mode (multi-room) WS/REST ─────────────────────────────────────

/// Shared state for daemon-mode axum handlers.
#[derive(Clone)]
pub(crate) struct DaemonWsState {
    pub(crate) rooms: super::daemon::RoomMap,
    pub(crate) next_client_id: Arc<AtomicU64>,
    pub(crate) config: super::daemon::DaemonConfig,
    pub(crate) system_token_map: super::state::TokenMap,
    pub(crate) user_registry: Arc<Mutex<crate::registry::UserRegistry>>,
}

impl DaemonWsState {
    /// Look up a room by ID. Returns the RoomState or None.
    pub(crate) async fn get_room(&self, room_id: &str) -> Option<Arc<RoomState>> {
        self.rooms.lock().await.get(room_id).cloned()
    }
}

/// Build the axum router for daemon mode (multi-room).
pub(crate) fn create_daemon_router(state: DaemonWsState) -> Router {
    Router::new()
        .route("/ws/{room_id}", get(daemon_ws_upgrade))
        .route("/api/{room_id}/join", post(rest::daemon_api_join))
        .route("/api/{room_id}/send", post(rest::daemon_api_send))
        .route("/api/{room_id}/poll", get(rest::daemon_api_poll))
        .route("/api/{room_id}/query", get(rest::daemon_api_query))
        .route("/api/health", get(rest::daemon_api_health))
        .route("/api/rooms", get(rest::daemon_api_rooms))
        .route("/api/rooms", post(rest::daemon_api_create_room))
        .with_state(state)
}

// ── Daemon WS upgrade ────────────────────────────────────────────────────

async fn daemon_ws_upgrade(
    ws: WebSocketUpgrade,
    Path(room_id): Path<String>,
    State(state): State<DaemonWsState>,
) -> impl IntoResponse {
    let room = match state.get_room(&room_id).await {
        Some(r) => r,
        None => {
            return (StatusCode::NOT_FOUND, "room not found").into_response();
        }
    };

    let next_id = state.next_client_id.clone();
    let registry = Some(state.user_registry.clone());
    ws.on_upgrade(move |socket| async move {
        let app_state = WsAppState {
            room_state: room,
            next_client_id: next_id,
            user_registry: registry,
        };
        if let Err(e) = handle_ws_client(socket, app_state).await {
            eprintln!("[daemon/ws] error: {e:#}");
        }
    })
}
