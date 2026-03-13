pub(crate) mod admin;
pub(crate) mod auth;
pub(crate) mod commands;
pub mod daemon;
pub(crate) mod fanout;
pub(crate) mod handshake;
pub(crate) mod persistence;
pub(crate) mod service;
pub(crate) mod state;
pub(crate) mod ws;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use crate::{
    history,
    message::{make_join, make_leave, make_system, parse_client_line, Message},
    plugin::PluginRegistry,
};
use auth::{handle_oneshot_join, validate_token};
use commands::{route_command, CommandResult};
use fanout::{broadcast_and_persist, dm_and_persist};
use room_protocol::SubscriptionTier;
use state::RoomState;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        unix::{OwnedReadHalf, OwnedWriteHalf},
        UnixListener, UnixStream,
    },
    sync::{broadcast, watch, Mutex},
};

/// Maximum bytes allowed in a single line from a client connection.
/// Prevents memory exhaustion from malicious clients sending arbitrarily large lines.
pub const MAX_LINE_BYTES: usize = 64 * 1024; // 64 KB

/// Read a single newline-terminated line, rejecting lines that exceed `MAX_LINE_BYTES`.
///
/// Returns `Ok(n)` where `n` is the number of bytes read (0 = EOF).
/// Returns an error if the accumulated bytes before a newline exceed the limit.
///
/// The line (including the trailing `\n`) is appended to `buf`, matching the
/// behaviour of `AsyncBufReadExt::read_line`.
pub(crate) async fn read_line_limited<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut String,
) -> anyhow::Result<usize> {
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            // EOF
            return Ok(total);
        }
        // Look for a newline in the buffered data.
        let (chunk, found_newline) = match available.iter().position(|&b| b == b'\n') {
            Some(pos) => (&available[..=pos], true),
            None => (available, false),
        };
        let chunk_len = chunk.len();
        if total + chunk_len > MAX_LINE_BYTES {
            anyhow::bail!("line exceeds maximum size of {} bytes", MAX_LINE_BYTES);
        }
        // Safety: we validate UTF-8 before appending.
        let text = std::str::from_utf8(chunk)
            .map_err(|e| anyhow::anyhow!("invalid UTF-8 in client line: {e}"))?;
        buf.push_str(text);
        total += chunk_len;
        reader.consume(chunk_len);
        if found_newline {
            return Ok(total);
        }
    }
}

pub struct Broker {
    room_id: String,
    chat_path: PathBuf,
    /// Path to the persisted token-map file (e.g. `~/.room/state/<room_id>.tokens`).
    token_map_path: PathBuf,
    /// Path to the persisted subscription-map file (e.g. `~/.room/state/<room_id>.subscriptions`).
    subscription_map_path: PathBuf,
    socket_path: PathBuf,
    ws_port: Option<u16>,
}

impl Broker {
    pub fn new(
        room_id: &str,
        chat_path: PathBuf,
        token_map_path: PathBuf,
        subscription_map_path: PathBuf,
        socket_path: PathBuf,
        ws_port: Option<u16>,
    ) -> Self {
        Self {
            room_id: room_id.to_owned(),
            chat_path,
            token_map_path,
            subscription_map_path,
            socket_path,
            ws_port,
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        // Remove stale socket synchronously — using tokio::fs here is dangerous
        // because the blocking thread pool may be shutting down if the broker
        // is starting up inside a dying process.
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        eprintln!("[broker] listening on {}", self.socket_path.display());

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let registry = PluginRegistry::with_all_plugins(&self.chat_path)?;

        // Load persisted state from a previous broker session (if any).
        let persisted_tokens = auth::load_token_map(&self.token_map_path);
        if !persisted_tokens.is_empty() {
            eprintln!(
                "[broker] loaded {} persisted token(s)",
                persisted_tokens.len()
            );
        }
        let persisted_subs = persistence::load_subscription_map(&self.subscription_map_path);
        if !persisted_subs.is_empty() {
            eprintln!(
                "[broker] loaded {} persisted subscription(s)",
                persisted_subs.len()
            );
        }

        let state = Arc::new(RoomState {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            auth: state::AuthState {
                token_map: Arc::new(Mutex::new(persisted_tokens)),
                token_map_path: Arc::new(self.token_map_path.clone()),
                registry: std::sync::OnceLock::new(),
            },
            filters: state::FilterState {
                subscription_map: Arc::new(Mutex::new(persisted_subs)),
                subscription_map_path: Arc::new(self.subscription_map_path.clone()),
                event_filter_state: std::sync::OnceLock::new(),
            },
            chat_path: Arc::new(self.chat_path.clone()),
            room_id: Arc::new(self.room_id.clone()),
            shutdown: Arc::new(shutdown_tx),
            seq_counter: Arc::new(AtomicU64::new(0)),
            plugin_registry: Arc::new(registry),
            config: None,
        });
        // Attach event filter map (parallel to subscription map).
        {
            let ef_path = self.subscription_map_path.with_extension("event_filters");
            let persisted_ef = persistence::load_event_filter_map(&ef_path);
            if !persisted_ef.is_empty() {
                eprintln!(
                    "[broker] loaded {} persisted event filter(s)",
                    persisted_ef.len()
                );
            }
            state.set_event_filter_map(Arc::new(Mutex::new(persisted_ef)), ef_path);
        }

        let next_client_id = Arc::new(AtomicU64::new(0));

        // Start WebSocket/REST server if a port was configured.
        if let Some(port) = self.ws_port {
            let ws_state = ws::WsAppState {
                room_state: state.clone(),
                next_client_id: next_client_id.clone(),
                user_registry: None,
            };
            let app = ws::create_router(ws_state);
            let tcp = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
            eprintln!("[broker] WebSocket/REST listening on port {port}");
            tokio::spawn(async move {
                if let Err(e) = axum::serve(tcp, app).await {
                    eprintln!("[broker] WS server error: {e}");
                }
            });
        }

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    let (stream, _) = accept?;
                    let cid = next_client_id.fetch_add(1, Ordering::SeqCst) + 1;

                    let (tx, _) = broadcast::channel::<String>(256);
                    // Insert with empty username; handle_client updates it after handshake.
                    state
                        .clients
                        .lock()
                        .await
                        .insert(cid, (String::new(), tx.clone()));

                    let state_clone = state.clone();

                    tokio::spawn(async move {
                        if let Err(e) = handle_client(cid, stream, tx, &state_clone).await {
                            eprintln!("[broker] client {cid} error: {e:#}");
                        }
                        state_clone.clients.lock().await.remove(&cid);
                    });
                }
                _ = shutdown_rx.changed() => {
                    eprintln!("[broker] shutdown requested, exiting");
                    break Ok(());
                }
            }
        }
    }
}

async fn handle_client(
    cid: u64,
    stream: UnixStream,
    own_tx: broadcast::Sender<String>,
    state: &Arc<RoomState>,
) -> anyhow::Result<()> {
    let token_map = state.auth.token_map.clone();

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // First line: username handshake, or one of the one-shot prefixes.
    let mut first = String::new();
    read_line_limited(&mut reader, &mut first).await?;
    let first_line = first.trim();

    use handshake::{parse_client_handshake, ClientHandshake};
    let username = match parse_client_handshake(first_line) {
        ClientHandshake::Send(u) => {
            eprintln!(
                "[broker] DEPRECATED: SEND:{u} handshake used — \
                 migrate to TOKEN:<uuid> (SEND: will be removed in a future version)"
            );
            return handle_oneshot_send(u, reader, write_half, state).await;
        }
        ClientHandshake::Token(token) => {
            return match validate_token(&token, &token_map).await {
                Some(u) => handle_oneshot_send(u, reader, write_half, state).await,
                None => {
                    let err = serde_json::json!({"type":"error","code":"invalid_token"});
                    write_half
                        .write_all(format!("{err}\n").as_bytes())
                        .await
                        .map_err(Into::into)
                }
            };
        }
        ClientHandshake::Join(u) => {
            let result = handle_oneshot_join(
                u,
                write_half,
                &token_map,
                &state.filters.subscription_map,
                state.config.as_ref(),
                Some(&state.auth.token_map_path),
            )
            .await;
            // Persist auto-subscription from join so it survives broker restart.
            persistence::persist_subscriptions(state).await;
            return result;
        }
        ClientHandshake::Session(token) => {
            return match validate_token(&token, &token_map).await {
                Some(u) => {
                    if let Err(reason) = auth::check_join_permission(&u, state.config.as_ref()) {
                        let err = serde_json::json!({
                            "type": "error",
                            "code": "join_denied",
                            "message": reason,
                            "username": u
                        });
                        write_half.write_all(format!("{err}\n").as_bytes()).await?;
                        return Ok(());
                    }
                    run_interactive_session(cid, &u, reader, write_half, own_tx, state).await
                }
                None => {
                    let err = serde_json::json!({"type":"error","code":"invalid_token"});
                    write_half
                        .write_all(format!("{err}\n").as_bytes())
                        .await
                        .map_err(Into::into)
                }
            };
        }
        ClientHandshake::Interactive(u) => {
            eprintln!(
                "[broker] DEPRECATED: unauthenticated interactive join for '{u}' — \
                 migrate to SESSION:<token> (plain username joins will be removed in a future version)"
            );
            u
        }
    };

    // Remaining path: deprecated unauthenticated interactive join.
    if username.is_empty() {
        return Ok(());
    }

    // Check join permission before entering interactive session.
    if let Err(reason) = auth::check_join_permission(&username, state.config.as_ref()) {
        let err = serde_json::json!({
            "type": "error",
            "code": "join_denied",
            "message": reason,
            "username": username
        });
        write_half.write_all(format!("{err}\n").as_bytes()).await?;
        return Ok(());
    }

    run_interactive_session(cid, &username, reader, write_half, own_tx, state).await
}

/// Run an interactive client session after the username has been determined.
///
/// Shared by both single-room (`handle_client`) and daemon (`dispatch_connection`)
/// paths. Handles: client registration, host election, history replay, join/leave
/// events, inbound/outbound message loops, and cleanup.
pub(crate) async fn run_interactive_session(
    cid: u64,
    username: &str,
    reader: BufReader<OwnedReadHalf>,
    mut write_half: OwnedWriteHalf,
    own_tx: broadcast::Sender<String>,
    state: &Arc<RoomState>,
) -> anyhow::Result<()> {
    let username = username.to_owned();

    // Register username in the client map
    {
        let mut map = state.clients.lock().await;
        if let Some(entry) = map.get_mut(&cid) {
            entry.0 = username.clone();
        }
    }

    // Register as host if no host has been set yet (first to complete handshake).
    // Persist the host username to the room meta file so oneshot commands (poll,
    // pull, query) can apply the same DM visibility rules without a live broker.
    {
        let mut host = state.host_user.lock().await;
        if host.is_none() {
            *host = Some(username.clone());
            let meta_path = crate::paths::room_meta_path(&state.room_id);
            if meta_path.exists() {
                if let Ok(data) = std::fs::read_to_string(&meta_path) {
                    if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&data) {
                        v["host"] = serde_json::Value::String(username.clone());
                        let _ = std::fs::write(&meta_path, v.to_string());
                    }
                }
            }
        }
    }

    eprintln!("[broker] {username} joined (cid={cid})");

    // Track this user in the status map (empty status by default)
    state
        .status_map
        .lock()
        .await
        .insert(username.clone(), String::new());

    // Subscribe before sending history so we don't miss concurrent messages
    let mut rx = own_tx.subscribe();

    // Send chat history directly to this client's socket, filtering DMs the
    // client is not party to (sender, recipient, or host).
    // If the client disconnects mid-replay, treat it as a clean exit.
    let host_name = state.host_user.lock().await.clone();
    let history = history::load(&state.chat_path).await.unwrap_or_default();
    for msg in &history {
        if msg.is_visible_to(&username, host_name.as_deref()) {
            let line = format!("{}\n", serde_json::to_string(msg)?);
            if write_half.write_all(line.as_bytes()).await.is_err() {
                return Ok(());
            }
        }
    }

    // Broadcast join event (also persists it)
    let join_msg = make_join(&state.room_id, &username);
    if let Err(e) = broadcast_and_persist(
        &join_msg,
        &state.clients,
        &state.chat_path,
        &state.seq_counter,
    )
    .await
    {
        eprintln!("[broker] broadcast_and_persist(join) failed: {e:#}");
        return Ok(());
    }
    state.plugin_registry.notify_join(&username);

    // Wrap write half in Arc<Mutex> for shared use by outbound and inbound tasks
    let write_half = Arc::new(Mutex::new(write_half));

    // Outbound: receive from broadcast channel, forward to client socket.
    // Also listens for the shutdown signal; drains the channel first so the
    // client sees the shutdown system message before receiving EOF.
    let write_half_out = write_half.clone();
    let mut shutdown_rx = state.shutdown.subscribe();
    let outbound = tokio::spawn(async move {
        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(line) => {
                            let mut wh = write_half_out.lock().await;
                            if wh.write_all(line.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("[broker] cid={cid} lagged by {n}");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = shutdown_rx.changed() => {
                    // Drain any messages already queued (e.g. the shutdown notice)
                    // before closing so the client sees them before receiving EOF.
                    while let Ok(line) = rx.try_recv() {
                        let mut wh = write_half_out.lock().await;
                        let _ = wh.write_all(line.as_bytes()).await;
                    }
                    // Explicitly shut down the write side to send EOF to the client,
                    // even though write_half_in (in the inbound task) still holds
                    // the Arc — without this, the socket stays open.
                    let _ = write_half_out.lock().await.shutdown().await;
                    break;
                }
            }
        }
    });

    // Inbound: read lines from client socket, parse and broadcast
    let username_in = username.clone();
    let room_id_in = state.room_id.clone();
    let write_half_in = write_half.clone();
    let state_in = state.clone();
    let inbound = tokio::spawn(async move {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match read_line_limited(&mut reader, &mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match parse_client_line(trimmed, &room_id_in, &username_in) {
                        Ok(msg) => match route_command(msg, &username_in, &state_in).await {
                            Ok(CommandResult::Handled | CommandResult::HandledWithReply(_)) => {}
                            Ok(CommandResult::Reply(json)) => {
                                let _ = write_half_in
                                    .lock()
                                    .await
                                    .write_all(format!("{json}\n").as_bytes())
                                    .await;
                            }
                            Ok(CommandResult::Shutdown) => break,
                            Ok(CommandResult::Passthrough(msg)) => {
                                // DM privacy: reject sends from non-participants
                                if let Err(reason) = auth::check_send_permission(
                                    &username_in,
                                    state_in.config.as_ref(),
                                ) {
                                    let err = serde_json::json!({
                                        "type": "error",
                                        "code": "send_denied",
                                        "message": reason
                                    });
                                    let _ = write_half_in
                                        .lock()
                                        .await
                                        .write_all(format!("{err}\n").as_bytes())
                                        .await;
                                    continue;
                                }
                                let is_broadcast = !matches!(&msg, Message::DirectMessage { .. });
                                // Subscribe @mentioned users BEFORE broadcast so the
                                // subscription is on disk before the message (#481).
                                let newly_subscribed = if is_broadcast {
                                    subscribe_mentioned(&msg, &state_in).await
                                } else {
                                    Vec::new()
                                };
                                let result = match &msg {
                                    Message::DirectMessage { .. } => {
                                        dm_and_persist(
                                            &msg,
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
                                if let Err(e) = &result {
                                    eprintln!("[broker] persist error: {e:#}");
                                }
                                if !newly_subscribed.is_empty() && result.is_ok() {
                                    broadcast_subscribe_notices(&newly_subscribed, &state_in).await;
                                }
                            }
                            Err(e) => eprintln!("[broker] route error: {e:#}"),
                        },
                        Err(e) => eprintln!("[broker] bad message from {username_in}: {e}"),
                    }
                }
                Err(e) => {
                    eprintln!("[broker] read error from {username_in}: {e:#}");
                    let err = serde_json::json!({
                        "type": "error",
                        "code": "line_too_long",
                        "message": format!("{e}")
                    });
                    let _ = write_half_in
                        .lock()
                        .await
                        .write_all(format!("{err}\n").as_bytes())
                        .await;
                    break;
                }
            }
        }
    });

    tokio::select! {
        _ = outbound => {},
        _ = inbound => {},
    }

    // Remove user from status map on disconnect
    state.status_map.lock().await.remove(&username);

    // Broadcast leave event
    let leave_msg = make_leave(&state.room_id, &username);
    let _ = broadcast_and_persist(
        &leave_msg,
        &state.clients,
        &state.chat_path,
        &state.seq_counter,
    )
    .await;
    state.plugin_registry.notify_leave(&username);
    eprintln!("[broker] {username} left (cid={cid})");

    Ok(())
}

/// Handle a one-shot SEND connection: read one message line, route it, echo it back, close.
/// The sender is never registered in ClientMap/StatusMap and generates no join/leave events.
/// DM envelopes are routed via `dm_and_persist`; all other messages are broadcast.
pub(crate) async fn handle_oneshot_send(
    username: String,
    mut reader: BufReader<OwnedReadHalf>,
    mut write_half: OwnedWriteHalf,
    state: &RoomState,
) -> anyhow::Result<()> {
    let mut line = String::new();
    read_line_limited(&mut reader, &mut line).await?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let msg = match parse_client_line(trimmed, &state.room_id, &username) {
        Ok(m) => m,
        Err(e) => {
            let err = serde_json::json!({
                "type": "error",
                "code": "parse_error",
                "message": format!("{e:#}")
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    };
    let cmd_result = match route_command(msg, &username, state).await {
        Ok(r) => r,
        Err(e) => {
            let err = serde_json::json!({
                "type": "error",
                "code": "route_error",
                "message": format!("{e:#}")
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    };
    match cmd_result {
        CommandResult::Handled | CommandResult::Shutdown => {
            // Always send a response so oneshot clients don't get EOF.
            let ack = make_system(&state.room_id, "broker", "ok");
            let json = serde_json::to_string(&ack)?;
            write_half.write_all(format!("{json}\n").as_bytes()).await?;
        }
        CommandResult::HandledWithReply(json) | CommandResult::Reply(json) => {
            write_half.write_all(format!("{json}\n").as_bytes()).await?;
        }
        CommandResult::Passthrough(msg) => {
            // DM privacy: reject sends from non-participants
            if let Err(reason) = auth::check_send_permission(&username, state.config.as_ref()) {
                let err = serde_json::json!({
                    "type": "error",
                    "code": "send_denied",
                    "message": reason
                });
                write_half.write_all(format!("{err}\n").as_bytes()).await?;
                return Ok(());
            }
            let is_broadcast = !matches!(&msg, Message::DirectMessage { .. });
            // Subscribe @mentioned users BEFORE broadcast so the
            // subscription is on disk before the message (#481).
            let newly_subscribed = if is_broadcast {
                subscribe_mentioned(&msg, state).await
            } else {
                Vec::new()
            };
            let seq_msg = match &msg {
                Message::DirectMessage { .. } => {
                    dm_and_persist(
                        &msg,
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
            if !newly_subscribed.is_empty() {
                broadcast_subscribe_notices(&newly_subscribed, state).await;
            }
            let echo = format!("{}\n", serde_json::to_string(&seq_msg)?);
            write_half.write_all(echo.as_bytes()).await?;
        }
    }
    Ok(())
}

/// Subscribe @mentioned users who are not already subscribed (or are `Unsubscribed`).
///
/// Must be called BEFORE `broadcast_and_persist` so that the subscription exists
/// on disk before the message is persisted to the chat file. This ensures poll-based
/// room discovery (`discover_joined_rooms`) finds the room before the mention message
/// is written, closing the race window described in #481.
///
/// Returns the list of newly subscribed usernames. Callers should pass this to
/// [`broadcast_subscribe_notices`] after the message has been broadcast.
async fn subscribe_mentioned(msg: &Message, state: &RoomState) -> Vec<String> {
    let mentioned = msg.mentions();
    if mentioned.is_empty() {
        return Vec::new();
    }

    // Collect users to auto-subscribe (brief lock hold).
    let newly_subscribed = {
        let token_map = state.auth.token_map.lock().await;
        let registered: std::collections::HashSet<&str> =
            token_map.values().map(String::as_str).collect();

        let mut sub_map = state.filters.subscription_map.lock().await;
        let mut newly = Vec::new();

        for username in &mentioned {
            if !registered.contains(username.as_str()) {
                continue;
            }
            let dominated = match sub_map.get(username.as_str()) {
                None | Some(SubscriptionTier::Unsubscribed) => true,
                Some(_) => false,
            };
            if dominated {
                sub_map.insert(username.clone(), SubscriptionTier::MentionsOnly);
                newly.push(username.clone());
            }
        }
        newly
    };

    if !newly_subscribed.is_empty() {
        // Persist the updated subscription map to disk so that
        // `discover_joined_rooms` picks up the new room immediately.
        persistence::persist_subscriptions(state).await;
    }

    newly_subscribed
}

/// Broadcast system notices for users that were auto-subscribed by [`subscribe_mentioned`].
///
/// Call this AFTER the original message has been broadcast so that the notice
/// appears after the mention in chat history.
async fn broadcast_subscribe_notices(newly_subscribed: &[String], state: &RoomState) {
    for username in newly_subscribed {
        let notice = format!(
            "{username} auto-subscribed at mentions_only (mentioned in {})",
            state.room_id
        );
        let sys = make_system(&state.room_id, "broker", notice);
        let _ =
            broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::make_message;
    use std::collections::HashMap;
    use tokio::sync::watch;

    fn make_test_state(chat_path: std::path::PathBuf) -> Arc<RoomState> {
        let (shutdown_tx, _) = watch::channel(false);
        Arc::new(RoomState {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            auth: state::AuthState {
                token_map: Arc::new(Mutex::new(HashMap::new())),
                token_map_path: Arc::new(chat_path.with_extension("tokens")),
                registry: std::sync::OnceLock::new(),
            },
            filters: state::FilterState {
                subscription_map: Arc::new(Mutex::new(HashMap::new())),
                subscription_map_path: Arc::new(chat_path.with_extension("subscriptions")),
                event_filter_state: std::sync::OnceLock::new(),
            },
            chat_path: Arc::new(chat_path.clone()),
            room_id: Arc::new("test-room".to_owned()),
            shutdown: Arc::new(shutdown_tx),
            seq_counter: Arc::new(AtomicU64::new(0)),
            plugin_registry: Arc::new(PluginRegistry::new()),
            config: None,
        })
    }

    #[tokio::test]
    async fn auto_subscribe_skips_unregistered_users() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        // Message mentions @alice but alice has no token — should not auto-subscribe.
        let msg = make_message("test-room", "bob", "hey @alice check this");
        subscribe_mentioned(&msg, &state).await;
        assert!(state.filters.subscription_map.lock().await.is_empty());
    }

    #[tokio::test]
    async fn auto_subscribe_registers_mentions_only_for_unsubscribed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        // Register alice in token map.
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hey @alice check this");
        subscribe_mentioned(&msg, &state).await;
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn auto_subscribe_skips_already_subscribed_full() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        state
            .filters
            .subscription_map
            .lock()
            .await
            .insert("alice".to_owned(), SubscriptionTier::Full);
        let msg = make_message("test-room", "bob", "hey @alice check this");
        subscribe_mentioned(&msg, &state).await;
        // Should remain Full, not downgraded to MentionsOnly.
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::Full
        );
    }

    #[tokio::test]
    async fn auto_subscribe_skips_already_subscribed_mentions_only() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        state
            .filters
            .subscription_map
            .lock()
            .await
            .insert("alice".to_owned(), SubscriptionTier::MentionsOnly);
        let msg = make_message("test-room", "bob", "@alice ping");
        subscribe_mentioned(&msg, &state).await;
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn auto_subscribe_upgrades_unsubscribed_to_mentions_only() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        state
            .filters
            .subscription_map
            .lock()
            .await
            .insert("alice".to_owned(), SubscriptionTier::Unsubscribed);
        let msg = make_message("test-room", "bob", "@alice come back");
        subscribe_mentioned(&msg, &state).await;
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn auto_subscribe_handles_multiple_mentions() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        {
            let mut tokens = state.auth.token_map.lock().await;
            tokens.insert("tok-alice".to_owned(), "alice".to_owned());
            tokens.insert("tok-carol".to_owned(), "carol".to_owned());
        }
        let msg = make_message("test-room", "bob", "@alice @carol @unknown review this");
        subscribe_mentioned(&msg, &state).await;
        let sub_map = state.filters.subscription_map.lock().await;
        assert_eq!(
            *sub_map.get("alice").unwrap(),
            SubscriptionTier::MentionsOnly
        );
        assert_eq!(
            *sub_map.get("carol").unwrap(),
            SubscriptionTier::MentionsOnly
        );
        assert!(sub_map.get("unknown").is_none());
    }

    #[tokio::test]
    async fn auto_subscribe_no_op_for_message_without_mentions() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hello everyone");
        subscribe_mentioned(&msg, &state).await;
        assert!(state.filters.subscription_map.lock().await.is_empty());
    }

    #[tokio::test]
    async fn auto_subscribe_broadcasts_notice() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hey @alice");
        let newly = subscribe_mentioned(&msg, &state).await;
        broadcast_subscribe_notices(&newly, &state).await;
        // Verify the auto-subscribe notice was persisted to chat history.
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("auto-subscribed"));
        assert!(history.contains("alice"));
        assert!(history.contains("mentions_only"));
    }

    #[tokio::test]
    async fn auto_subscribe_persists_to_disk() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hey @alice");
        subscribe_mentioned(&msg, &state).await;
        // Verify subscriptions were persisted to the .subscriptions file.
        let loaded = persistence::load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::MentionsOnly));
    }

    /// Regression test for #481: subscription must be persisted to disk BEFORE
    /// the message is written to the chat file. This ensures `discover_joined_rooms`
    /// finds the room before the mention message appears in history.
    #[tokio::test]
    async fn subscribe_mentioned_returns_newly_subscribed_before_broadcast() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hey @alice check this");

        // Step 1: subscribe_mentioned runs before broadcast — subscription is on disk.
        let newly = subscribe_mentioned(&msg, &state).await;
        assert_eq!(newly, vec!["alice"]);

        // Verify subscription is persisted BEFORE any message is in the chat file.
        let loaded = persistence::load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::MentionsOnly));
        // Chat file should still be empty (broadcast hasn't happened yet).
        let chat_content = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            chat_content.is_empty(),
            "chat file must be empty before broadcast — subscription should precede message"
        );

        // Step 2: broadcast the message (simulating the real flow).
        let seq_msg =
            broadcast_and_persist(&msg, &state.clients, &state.chat_path, &state.seq_counter)
                .await
                .unwrap();
        assert!(seq_msg.seq().is_some());

        // Step 3: broadcast notices after the message.
        broadcast_subscribe_notices(&newly, &state).await;

        // Verify ordering: chat file has the message, then the notice.
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        let lines: Vec<&str> = history.trim().lines().collect();
        assert_eq!(lines.len(), 2, "expected message + notice");
        assert!(lines[0].contains("hey @alice check this"));
        assert!(lines[1].contains("auto-subscribed"));
    }

    // --- read_line_limited tests ---

    #[tokio::test]
    async fn read_line_limited_reads_normal_line() {
        let data = b"hello world\n";
        let cursor = std::io::Cursor::new(data.to_vec());
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, 12);
        assert_eq!(buf, "hello world\n");
    }

    #[tokio::test]
    async fn read_line_limited_reads_line_without_trailing_newline() {
        let data = b"no newline";
        let cursor = std::io::Cursor::new(data.to_vec());
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, 10);
        assert_eq!(buf, "no newline");
    }

    #[tokio::test]
    async fn read_line_limited_returns_zero_on_eof() {
        let data = b"";
        let cursor = std::io::Cursor::new(data.to_vec());
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, 0);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn read_line_limited_rejects_oversized_line() {
        // Create a line that exceeds the limit (no newline, so it keeps reading).
        let data = vec![b'A'; MAX_LINE_BYTES + 1];
        let cursor = std::io::Cursor::new(data);
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let result = read_line_limited(&mut reader, &mut buf).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds maximum size"),
            "unexpected error: {err_msg}"
        );
    }

    #[tokio::test]
    async fn read_line_limited_accepts_line_at_exact_limit() {
        // Line of exactly MAX_LINE_BYTES (including the newline).
        let mut data = vec![b'A'; MAX_LINE_BYTES - 1];
        data.push(b'\n');
        let cursor = std::io::Cursor::new(data);
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, MAX_LINE_BYTES);
        assert!(buf.ends_with('\n'));
    }

    #[tokio::test]
    async fn read_line_limited_reads_multiple_lines() {
        let data = b"line one\nline two\n";
        let cursor = std::io::Cursor::new(data.to_vec());
        let mut reader = tokio::io::BufReader::new(cursor);

        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, 9);
        assert_eq!(buf, "line one\n");

        buf.clear();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, 9);
        assert_eq!(buf, "line two\n");

        buf.clear();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn read_line_limited_rejects_invalid_utf8() {
        let data: Vec<u8> = vec![0xFF, 0xFE, b'\n'];
        let cursor = std::io::Cursor::new(data);
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let result = read_line_limited(&mut reader, &mut buf).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid UTF-8"),
            "unexpected error: {err_msg}"
        );
    }
}
