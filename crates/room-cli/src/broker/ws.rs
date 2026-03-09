use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{broadcast, Mutex};

use crate::{
    history,
    message::{make_dm, make_join, make_leave, make_message, parse_client_line, Message},
};

use super::{
    auth::{check_join_permission, check_send_permission, issue_token, validate_token},
    commands::{route_command, CommandResult},
    fanout::{broadcast_and_persist, dm_and_persist},
    state::RoomState,
};

/// Shared state for axum handlers.
#[derive(Clone)]
pub(crate) struct WsAppState {
    pub(crate) room_state: Arc<RoomState>,
    pub(crate) next_client_id: Arc<AtomicU64>,
}

/// Build the axum router with WebSocket and REST routes.
pub(crate) fn create_router(state: WsAppState) -> Router {
    Router::new()
        .route("/ws/{room_id}", get(ws_upgrade_handler))
        .route("/api/{room_id}/join", post(api_join))
        .route("/api/{room_id}/send", post(api_send))
        .route("/api/{room_id}/poll", get(api_poll))
        .route("/api/health", get(api_health))
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
    let cid = app_state.next_client_id.fetch_add(1, Ordering::SeqCst) + 1;

    let (tx, _) = broadcast::channel::<String>(256);
    state
        .clients
        .lock()
        .await
        .insert(cid, (String::new(), tx.clone()));

    let result = run_ws_session(cid, ws, tx, &state).await;
    state.clients.lock().await.remove(&cid);
    result
}

async fn run_ws_session(
    cid: u64,
    ws: WebSocket,
    own_tx: broadcast::Sender<String>,
    state: &Arc<RoomState>,
) -> anyhow::Result<()> {
    let (mut ws_tx, mut ws_rx) = ws.split();

    // Read first frame: handshake (same protocol as UDS).
    let first_frame = match ws_rx.next().await {
        Some(Ok(WsMessage::Text(text))) => text.trim().to_owned(),
        Some(Ok(WsMessage::Close(_))) | None => return Ok(()),
        Some(Ok(_)) => return Ok(()),
        Some(Err(e)) => return Err(e.into()),
    };

    // One-shot: JOIN — register username, return token, close.
    if let Some(join_user) = first_frame.strip_prefix("JOIN:") {
        return ws_oneshot_join(join_user, &mut ws_tx, state).await;
    }

    // One-shot: SEND — legacy unauthenticated send.
    if let Some(send_user) = first_frame.strip_prefix("SEND:") {
        return ws_oneshot_send(send_user.to_owned(), &mut ws_rx, &mut ws_tx, state).await;
    }

    // One-shot: TOKEN — authenticated send.
    if let Some(token) = first_frame.strip_prefix("TOKEN:") {
        return match validate_token(token, &state.token_map).await {
            Some(u) => ws_oneshot_send(u, &mut ws_rx, &mut ws_tx, state).await,
            None => {
                let err = serde_json::json!({"type":"error","code":"invalid_token"});
                let _ = ws_tx.send(WsMessage::Text(err.to_string().into())).await;
                Ok(())
            }
        };
    }

    // Interactive join — first_frame is the username.
    let username = first_frame;
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

    // Register username in client map.
    {
        let mut map = state.clients.lock().await;
        if let Some(entry) = map.get_mut(&cid) {
            entry.0 = username.clone();
        }
    }

    // First interactive join becomes host.
    {
        let mut host = state.host_user.lock().await;
        if host.is_none() {
            *host = Some(username.clone());
        }
    }

    eprintln!("[broker/ws] {username} joined (cid={cid})");

    state
        .status_map
        .lock()
        .await
        .insert(username.clone(), String::new());

    // Subscribe before sending history so we don't miss concurrent messages.
    let mut rx = own_tx.subscribe();

    // Send chat history, filtering DMs the client is not party to.
    let host_name = state.host_user.lock().await.clone();
    let is_host = host_name.as_deref() == Some(username.as_str());
    let history = history::load(&state.chat_path).await.unwrap_or_default();
    for msg in &history {
        let visible = match msg {
            Message::DirectMessage { user, to, .. } => {
                is_host || user == &username || to == &username
            }
            _ => true,
        };
        if visible {
            let line = serde_json::to_string(msg)?;
            if ws_tx.send(WsMessage::Text(line.into())).await.is_err() {
                return Ok(());
            }
        }
    }

    // Broadcast join event.
    let join_msg = make_join(&state.room_id, &username);
    if let Err(e) = broadcast_and_persist(
        &join_msg,
        &state.clients,
        &state.chat_path,
        &state.seq_counter,
    )
    .await
    {
        eprintln!("[broker/ws] broadcast_and_persist(join) failed: {e:#}");
        return Ok(());
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

    // Inbound: read text frames, parse, and route.
    let username_in = username.clone();
    let room_id_in = state.room_id.clone();
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
                    match parse_client_line(trimmed, &room_id_in, &username_in) {
                        Ok(msg) => match route_command(msg, &username_in, &state_in).await {
                            Ok(CommandResult::Handled | CommandResult::HandledWithReply(_)) => {}
                            Ok(CommandResult::Reply(json)) => {
                                let _ = ws_tx_in
                                    .lock()
                                    .await
                                    .send(WsMessage::Text(json.into()))
                                    .await;
                            }
                            Ok(CommandResult::Shutdown) => break,
                            Ok(CommandResult::Passthrough(msg)) => {
                                // DM privacy: reject sends from non-participants.
                                if let Err(reason) =
                                    check_send_permission(&username_in, state_in.config.as_ref())
                                {
                                    let err = serde_json::json!({
                                        "type": "error",
                                        "code": "send_denied",
                                        "message": reason
                                    });
                                    let _ = ws_tx_in
                                        .lock()
                                        .await
                                        .send(WsMessage::Text(err.to_string().into()))
                                        .await;
                                    continue;
                                }
                                let result = match &msg {
                                    Message::DirectMessage { to, .. } => {
                                        dm_and_persist(
                                            &msg,
                                            &username_in,
                                            to,
                                            &state_in.host_user,
                                            &state_in.clients,
                                            &state_in.chat_path,
                                            &state_in.seq_counter,
                                        )
                                        .await
                                    }
                                    _ => {
                                        broadcast_and_persist(
                                            &msg,
                                            &state_in.clients,
                                            &state_in.chat_path,
                                            &state_in.seq_counter,
                                        )
                                        .await
                                    }
                                };
                                if let Err(e) = result {
                                    eprintln!("[broker/ws] persist error: {e:#}");
                                }
                            }
                            Err(e) => eprintln!("[broker/ws] route error: {e:#}"),
                        },
                        Err(e) => eprintln!("[broker/ws] bad message from {username_in}: {e}"),
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

    // Cleanup.
    state.status_map.lock().await.remove(&username);
    let leave_msg = make_leave(&state.room_id, &username);
    let _ = broadcast_and_persist(
        &leave_msg,
        &state.clients,
        &state.chat_path,
        &state.seq_counter,
    )
    .await;
    eprintln!("[broker/ws] {username} left (cid={cid})");

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
    match issue_token(username, &state.token_map, Some(&state.token_map_path)).await {
        Ok(token) => {
            let resp = serde_json::json!({"type":"token","token": token, "username": username});
            let _ = ws_tx.send(WsMessage::Text(resp.to_string().into())).await;
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
    let msg = parse_client_line(trimmed, &state.room_id, &username)?;
    match route_command(msg, &username, state).await? {
        CommandResult::Handled | CommandResult::Shutdown => {}
        CommandResult::HandledWithReply(json) | CommandResult::Reply(json) => {
            let _ = ws_tx.send(WsMessage::Text(json.into())).await;
        }
        CommandResult::Passthrough(msg) => {
            // DM privacy: reject sends from non-participants.
            if let Err(reason) = check_send_permission(&username, state.config.as_ref()) {
                let err = serde_json::json!({
                    "type": "error",
                    "code": "send_denied",
                    "message": reason
                });
                let _ = ws_tx.send(WsMessage::Text(err.to_string().into())).await;
                return Ok(());
            }
            let seq_msg = match &msg {
                Message::DirectMessage { to, .. } => {
                    dm_and_persist(
                        &msg,
                        &username,
                        to,
                        &state.host_user,
                        &state.clients,
                        &state.chat_path,
                        &state.seq_counter,
                    )
                    .await?
                }
                _ => {
                    broadcast_and_persist(
                        &msg,
                        &state.clients,
                        &state.chat_path,
                        &state.seq_counter,
                    )
                    .await?
                }
            };
            let echo = serde_json::to_string(&seq_msg)?;
            let _ = ws_tx.send(WsMessage::Text(echo.into())).await;
        }
    }
    Ok(())
}

// ── REST endpoints ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct JoinRequest {
    username: String,
}

async fn api_join(
    Path(room_id): Path<String>,
    State(state): State<WsAppState>,
    Json(body): Json<JoinRequest>,
) -> impl IntoResponse {
    if room_id != *state.room_state.room_id {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"type":"error","code":"room_not_found"})),
        )
            .into_response();
    }
    if let Err(reason) =
        super::auth::check_join_permission(&body.username, state.room_state.config.as_ref())
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"type":"error","code":"join_denied","message": reason})),
        )
            .into_response();
    }
    match issue_token(
        &body.username,
        &state.room_state.token_map,
        Some(&state.room_state.token_map_path),
    )
    .await
    {
        Ok(token) => {
            let resp = serde_json::json!({
                "type":"token","token": token, "username": body.username
            });
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(_) => {
            let err = serde_json::json!({
                "type":"error","code":"username_taken","username": body.username
            });
            (StatusCode::CONFLICT, Json(err)).into_response()
        }
    }
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
}

#[derive(Deserialize)]
struct SendRequest {
    content: String,
    to: Option<String>,
}

async fn api_send(
    Path(room_id): Path<String>,
    State(state): State<WsAppState>,
    headers: HeaderMap,
    Json(body): Json<SendRequest>,
) -> impl IntoResponse {
    if room_id != *state.room_state.room_id {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"type":"error","code":"room_not_found"})),
        )
            .into_response();
    }

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match validate_token(token, &state.room_state.token_map).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let rs = &state.room_state;
    let msg = if let Some(ref to) = body.to {
        make_dm(&rs.room_id, &username, to, &body.content)
    } else {
        make_message(&rs.room_id, &username, &body.content)
    };

    match route_command(msg, &username, rs).await {
        Ok(CommandResult::Handled) => {
            (StatusCode::OK, Json(serde_json::json!({"type":"ok"}))).into_response()
        }
        Ok(CommandResult::HandledWithReply(json) | CommandResult::Reply(json)) => {
            let v: serde_json::Value =
                serde_json::from_str(&json).unwrap_or(serde_json::json!({"reply": json}));
            (StatusCode::OK, Json(v)).into_response()
        }
        Ok(CommandResult::Shutdown) => {
            (StatusCode::OK, Json(serde_json::json!({"type":"shutdown"}))).into_response()
        }
        Ok(CommandResult::Passthrough(msg)) => {
            // DM privacy: reject sends from non-participants.
            if let Err(reason) = check_send_permission(&username, rs.config.as_ref()) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(
                        serde_json::json!({"type":"error","code":"send_denied","message": reason}),
                    ),
                )
                    .into_response();
            }
            let result = match &msg {
                Message::DirectMessage { to, .. } => {
                    dm_and_persist(
                        &msg,
                        &username,
                        to,
                        &rs.host_user,
                        &rs.clients,
                        &rs.chat_path,
                        &rs.seq_counter,
                    )
                    .await
                }
                _ => broadcast_and_persist(&msg, &rs.clients, &rs.chat_path, &rs.seq_counter).await,
            };
            match result {
                Ok(seq_msg) => {
                    let json = serde_json::to_value(&seq_msg).unwrap_or_default();
                    (StatusCode::OK, Json(json)).into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"type":"error","code":"persist_error","message": e.to_string()})),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"type":"error","code":"route_error","message": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct PollQuery {
    since: Option<String>,
}

async fn api_poll(
    Path(room_id): Path<String>,
    State(state): State<WsAppState>,
    headers: HeaderMap,
    Query(query): Query<PollQuery>,
) -> impl IntoResponse {
    if room_id != *state.room_state.room_id {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"type":"error","code":"room_not_found"})),
        )
            .into_response();
    }

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match validate_token(token, &state.room_state.token_map).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let rs = &state.room_state;
    let history = history::load(&rs.chat_path).await.unwrap_or_default();
    let host_name = rs.host_user.lock().await.clone();
    let is_host = host_name.as_deref() == Some(username.as_str());

    // Filter messages after the `since` ID (stateless — no server-side cursor).
    let mut found_since = query.since.is_none();
    let messages: Vec<serde_json::Value> = history
        .into_iter()
        .filter(|msg| {
            if !found_since {
                if msg.id() == query.since.as_deref().unwrap_or_default() {
                    found_since = true;
                }
                return false;
            }
            match msg {
                Message::DirectMessage { user, to, .. } => {
                    is_host || user == &username || to == &username
                }
                _ => true,
            }
        })
        .filter_map(|msg| serde_json::to_value(&msg).ok())
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({ "messages": messages })),
    )
        .into_response()
}

async fn api_health(State(state): State<WsAppState>) -> impl IntoResponse {
    let users = state.room_state.status_map.lock().await.len();
    Json(serde_json::json!({
        "status": "ok",
        "room": *state.room_state.room_id,
        "users": users
    }))
}

// ── Daemon-mode (multi-room) WS/REST ─────────────────────────────────────

/// Shared state for daemon-mode axum handlers.
#[derive(Clone)]
pub(crate) struct DaemonWsState {
    pub(crate) rooms: super::daemon::RoomMap,
    pub(crate) next_client_id: Arc<AtomicU64>,
}

impl DaemonWsState {
    /// Look up a room by ID. Returns the RoomState or None.
    async fn get_room(&self, room_id: &str) -> Option<Arc<RoomState>> {
        self.rooms.lock().await.get(room_id).cloned()
    }
}

/// Build the axum router for daemon mode (multi-room).
pub(crate) fn create_daemon_router(state: DaemonWsState) -> Router {
    Router::new()
        .route("/ws/{room_id}", get(daemon_ws_upgrade))
        .route("/api/{room_id}/join", post(daemon_api_join))
        .route("/api/{room_id}/send", post(daemon_api_send))
        .route("/api/{room_id}/poll", get(daemon_api_poll))
        .route("/api/health", get(daemon_api_health))
        .route("/api/rooms", get(daemon_api_rooms))
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
    ws.on_upgrade(move |socket| async move {
        let app_state = WsAppState {
            room_state: room,
            next_client_id: next_id,
        };
        if let Err(e) = handle_ws_client(socket, app_state).await {
            eprintln!("[daemon/ws] error: {e:#}");
        }
    })
}

// ── Daemon REST endpoints ────────────────────────────────────────────────

async fn daemon_api_join(
    Path(room_id): Path<String>,
    State(state): State<DaemonWsState>,
    Json(body): Json<JoinRequest>,
) -> impl IntoResponse {
    let room = match state.get_room(&room_id).await {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"type":"error","code":"room_not_found"})),
            )
                .into_response();
        }
    };
    if let Err(reason) = super::auth::check_join_permission(&body.username, room.config.as_ref()) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"type":"error","code":"join_denied","message": reason})),
        )
            .into_response();
    }
    match issue_token(&body.username, &room.token_map, Some(&room.token_map_path)).await {
        Ok(token) => {
            let resp = serde_json::json!({
                "type":"token","token": token, "username": body.username
            });
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(_) => {
            let err = serde_json::json!({
                "type":"error","code":"username_taken","username": body.username
            });
            (StatusCode::CONFLICT, Json(err)).into_response()
        }
    }
}

async fn daemon_api_send(
    Path(room_id): Path<String>,
    State(state): State<DaemonWsState>,
    headers: HeaderMap,
    Json(body): Json<SendRequest>,
) -> impl IntoResponse {
    let room = match state.get_room(&room_id).await {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"type":"error","code":"room_not_found"})),
            )
                .into_response();
        }
    };

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match validate_token(token, &room.token_map).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let msg = if let Some(ref to) = body.to {
        make_dm(&room.room_id, &username, to, &body.content)
    } else {
        make_message(&room.room_id, &username, &body.content)
    };

    match route_command(msg, &username, &room).await {
        Ok(CommandResult::Handled) => {
            (StatusCode::OK, Json(serde_json::json!({"type":"ok"}))).into_response()
        }
        Ok(CommandResult::HandledWithReply(json) | CommandResult::Reply(json)) => {
            let v: serde_json::Value =
                serde_json::from_str(&json).unwrap_or(serde_json::json!({"reply": json}));
            (StatusCode::OK, Json(v)).into_response()
        }
        Ok(CommandResult::Shutdown) => {
            (StatusCode::OK, Json(serde_json::json!({"type":"shutdown"}))).into_response()
        }
        Ok(CommandResult::Passthrough(msg)) => {
            // DM privacy: reject sends from non-participants.
            if let Err(reason) = check_send_permission(&username, room.config.as_ref()) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(
                        serde_json::json!({"type":"error","code":"send_denied","message": reason}),
                    ),
                )
                    .into_response();
            }
            let result = match &msg {
                Message::DirectMessage { to, .. } => {
                    dm_and_persist(
                        &msg,
                        &username,
                        to,
                        &room.host_user,
                        &room.clients,
                        &room.chat_path,
                        &room.seq_counter,
                    )
                    .await
                }
                _ => {
                    broadcast_and_persist(&msg, &room.clients, &room.chat_path, &room.seq_counter)
                        .await
                }
            };
            match result {
                Ok(seq_msg) => {
                    let json = serde_json::to_value(&seq_msg).unwrap_or_default();
                    (StatusCode::OK, Json(json)).into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"type":"error","code":"persist_error","message": e.to_string()})),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"type":"error","code":"route_error","message": e.to_string()})),
        )
            .into_response(),
    }
}

async fn daemon_api_poll(
    Path(room_id): Path<String>,
    State(state): State<DaemonWsState>,
    headers: HeaderMap,
    Query(query): Query<PollQuery>,
) -> impl IntoResponse {
    let room = match state.get_room(&room_id).await {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"type":"error","code":"room_not_found"})),
            )
                .into_response();
        }
    };

    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"missing_token"})),
            )
                .into_response()
        }
    };

    let username = match validate_token(token, &room.token_map).await {
        Some(u) => u,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"type":"error","code":"invalid_token"})),
            )
                .into_response()
        }
    };

    let history = history::load(&room.chat_path).await.unwrap_or_default();
    let host_name = room.host_user.lock().await.clone();
    let is_host = host_name.as_deref() == Some(username.as_str());

    let mut found_since = query.since.is_none();
    let messages: Vec<serde_json::Value> = history
        .into_iter()
        .filter(|msg| {
            if !found_since {
                if msg.id() == query.since.as_deref().unwrap_or_default() {
                    found_since = true;
                }
                return false;
            }
            match msg {
                Message::DirectMessage { user, to, .. } => {
                    is_host || user == &username || to == &username
                }
                _ => true,
            }
        })
        .filter_map(|msg| serde_json::to_value(&msg).ok())
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({ "messages": messages })),
    )
        .into_response()
}

async fn daemon_api_health(State(state): State<DaemonWsState>) -> impl IntoResponse {
    let rooms = state.rooms.lock().await;
    let mut room_info = Vec::new();
    for (id, rs) in rooms.iter() {
        let users = rs.status_map.lock().await.len();
        room_info.push(serde_json::json!({
            "room": id,
            "users": users,
        }));
    }
    Json(serde_json::json!({
        "status": "ok",
        "rooms": room_info,
    }))
}

async fn daemon_api_rooms(State(state): State<DaemonWsState>) -> impl IntoResponse {
    let rooms = state.rooms.lock().await;
    let ids: Vec<&String> = rooms.keys().collect();
    Json(serde_json::json!({
        "rooms": ids,
    }))
}
