//! Multi-room daemon: manages N rooms in a single process.
//!
//! `DaemonState` wraps a map of room_id → `RoomState` and provides room
//! lifecycle (create/destroy/get). The daemon listens on a single UDS
//! socket at a configurable path and dispatches connections to the correct
//! room based on an extended handshake protocol.
//!
//! ## Handshake protocol
//!
//! The first line of a UDS connection to the daemon can optionally carry a
//! `ROOM:<room_id>:` prefix. If present, the rest of the line is interpreted
//! as the standard per-room handshake (`SEND:`, `TOKEN:`, `JOIN:`, or plain
//! username). If absent, the connection is rejected with an error envelope.
//!
//! Examples:
//! ```text
//! ROOM:myroom:JOIN:alice       → join room "myroom" as "alice"
//! ROOM:myroom:TOKEN:<uuid>     → authenticated send to "myroom"
//! ROOM:myroom:SEND:bob         → legacy unauthenticated send to "myroom"
//! ROOM:myroom:alice            → interactive join to "myroom" as "alice"
//! ```

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use tokio::{
    io::BufReader,
    net::UnixListener,
    sync::{broadcast, watch, Mutex},
};

use crate::plugin::{self, PluginRegistry};

use super::{
    handle_oneshot_send,
    state::RoomState,
    ws::{self, DaemonWsState},
};

/// Configuration for the daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Path to the daemon UDS socket.
    pub socket_path: PathBuf,
    /// Directory for chat files. Each room gets `<data_dir>/<room_id>.chat`.
    pub data_dir: PathBuf,
    /// Optional WebSocket/REST port.
    pub ws_port: Option<u16>,
}

impl DaemonConfig {
    /// Resolve the chat file path for a given room.
    pub fn chat_path(&self, room_id: &str) -> PathBuf {
        self.data_dir.join(format!("{room_id}.chat"))
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: PathBuf::from("/tmp/roomd.sock"),
            data_dir: PathBuf::from("/tmp"),
            ws_port: None,
        }
    }
}

/// Registry of active rooms, keyed by room_id.
pub(crate) type RoomMap = Arc<Mutex<HashMap<String, Arc<RoomState>>>>;

/// Multi-room daemon state.
pub struct DaemonState {
    pub(crate) rooms: RoomMap,
    pub(crate) config: DaemonConfig,
    /// Global client ID counter shared across all rooms.
    pub(crate) next_client_id: Arc<AtomicU64>,
    /// Daemon-level shutdown signal.
    pub(crate) shutdown: Arc<watch::Sender<bool>>,
}

impl DaemonState {
    /// Create a new daemon with the given configuration and no rooms.
    pub fn new(config: DaemonConfig) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            rooms: Arc::new(Mutex::new(HashMap::new())),
            config,
            next_client_id: Arc::new(AtomicU64::new(0)),
            shutdown: Arc::new(shutdown_tx),
        }
    }

    /// Create a room and register it. Returns `Err` if the room already exists.
    pub async fn create_room(&self, room_id: &str) -> Result<(), String> {
        let mut rooms = self.rooms.lock().await;
        if rooms.contains_key(room_id) {
            return Err(format!("room already exists: {room_id}"));
        }

        let chat_path = self.config.chat_path(room_id);
        let (shutdown_tx, _) = watch::channel(false);

        let mut registry = PluginRegistry::new();
        registry
            .register(Box::new(plugin::help::HelpPlugin))
            .map_err(|e| format!("plugin error: {e}"))?;
        registry
            .register(Box::new(plugin::stats::StatsPlugin))
            .map_err(|e| format!("plugin error: {e}"))?;

        let state = Arc::new(RoomState {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            token_map: Arc::new(Mutex::new(HashMap::new())),
            chat_path: Arc::new(chat_path),
            room_id: Arc::new(room_id.to_owned()),
            shutdown: Arc::new(shutdown_tx),
            seq_counter: Arc::new(AtomicU64::new(0)),
            plugin_registry: Arc::new(registry),
        });

        rooms.insert(room_id.to_owned(), state);
        Ok(())
    }

    /// Destroy a room. Returns `Err` if the room does not exist.
    ///
    /// Signals the room's shutdown so connected clients receive EOF.
    pub async fn destroy_room(&self, room_id: &str) -> Result<(), String> {
        let mut rooms = self.rooms.lock().await;
        let state = rooms
            .remove(room_id)
            .ok_or_else(|| format!("room not found: {room_id}"))?;

        // Signal the room's shutdown so any connected clients receive EOF.
        let _ = state.shutdown.send(true);
        Ok(())
    }

    /// Check if a room exists.
    pub async fn has_room(&self, room_id: &str) -> bool {
        self.rooms.lock().await.contains_key(room_id)
    }

    /// Get a handle to the daemon-level shutdown sender.
    pub fn shutdown_handle(&self) -> Arc<watch::Sender<bool>> {
        self.shutdown.clone()
    }

    /// List all active room IDs.
    pub async fn list_rooms(&self) -> Vec<String> {
        self.rooms.lock().await.keys().cloned().collect()
    }

    /// Run the daemon: listen on UDS, dispatch connections to rooms.
    pub async fn run(&self) -> anyhow::Result<()> {
        // Remove stale socket synchronously.
        if self.config.socket_path.exists() {
            std::fs::remove_file(&self.config.socket_path)?;
        }

        let listener = UnixListener::bind(&self.config.socket_path)?;
        eprintln!(
            "[daemon] listening on {}",
            self.config.socket_path.display()
        );

        let mut shutdown_rx = self.shutdown.subscribe();

        // Start WebSocket/REST server if configured.
        if let Some(port) = self.config.ws_port {
            let ws_state = DaemonWsState {
                rooms: self.rooms.clone(),
                next_client_id: self.next_client_id.clone(),
            };
            let app = ws::create_daemon_router(ws_state);
            let tcp = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
            eprintln!("[daemon] WebSocket/REST listening on port {port}");
            tokio::spawn(async move {
                if let Err(e) = axum::serve(tcp, app).await {
                    eprintln!("[daemon] WS server error: {e}");
                }
            });
        }

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    let (stream, _) = accept?;
                    let rooms = self.rooms.clone();
                    let next_id = self.next_client_id.clone();

                    tokio::spawn(async move {
                        if let Err(e) = dispatch_connection(stream, &rooms, &next_id).await {
                            eprintln!("[daemon] connection error: {e:#}");
                        }
                    });
                }
                _ = shutdown_rx.changed() => {
                    eprintln!("[daemon] shutdown requested, exiting");
                    break Ok(());
                }
            }
        }
    }
}

/// Parse the `ROOM:<room_id>:` prefix from a handshake line.
///
/// Returns `(room_id, rest)` on success. `rest` is the remainder after the
/// second colon (e.g. `JOIN:alice`, `TOKEN:uuid`, `SEND:bob`, or `username`).
pub(crate) fn parse_room_prefix(line: &str) -> Option<(&str, &str)> {
    let stripped = line.strip_prefix("ROOM:")?;
    let colon = stripped.find(':')?;
    let room_id = &stripped[..colon];
    let rest = &stripped[colon + 1..];
    if room_id.is_empty() {
        return None;
    }
    Some((room_id, rest))
}

/// Dispatch a raw UDS connection to the correct room based on the handshake.
async fn dispatch_connection(
    stream: tokio::net::UnixStream,
    rooms: &RoomMap,
    next_client_id: &Arc<AtomicU64>,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let mut first = String::new();
    reader.read_line(&mut first).await?;
    let first_line = first.trim();

    if first_line.is_empty() {
        return Ok(());
    }

    // Parse ROOM:<room_id>:<rest>
    let (room_id, rest) = match parse_room_prefix(first_line) {
        Some(pair) => pair,
        None => {
            let err = serde_json::json!({
                "type": "error",
                "code": "missing_room_prefix",
                "message": "daemon mode requires ROOM:<room_id>: prefix in handshake"
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    };

    // Look up the room.
    let state = {
        let map = rooms.lock().await;
        map.get(room_id).cloned()
    };

    let state = match state {
        Some(s) => s,
        None => {
            let err = serde_json::json!({
                "type": "error",
                "code": "room_not_found",
                "room": room_id
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
            return Ok(());
        }
    };

    let cid = next_client_id.fetch_add(1, Ordering::SeqCst) + 1;

    // Dispatch based on the handshake after the ROOM: prefix.
    if let Some(send_user) = rest.strip_prefix("SEND:") {
        return handle_oneshot_send(send_user.to_owned(), reader, write_half, &state).await;
    }

    if let Some(token) = rest.strip_prefix("TOKEN:") {
        return match super::auth::validate_token(token, &state.token_map).await {
            Some(u) => handle_oneshot_send(u, reader, write_half, &state).await,
            None => {
                let err = serde_json::json!({"type":"error","code":"invalid_token"});
                write_half
                    .write_all(format!("{err}\n").as_bytes())
                    .await
                    .map_err(Into::into)
            }
        };
    }

    if let Some(join_user) = rest.strip_prefix("JOIN:") {
        return super::auth::handle_oneshot_join(
            join_user.to_owned(),
            write_half,
            &state.token_map,
        )
        .await;
    }

    // Interactive join: rest is the username.
    let username = rest;
    if username.is_empty() {
        return Ok(());
    }

    // Register client in room, then hand off to the full interactive handler.
    let (tx, _) = broadcast::channel::<String>(256);
    state
        .clients
        .lock()
        .await
        .insert(cid, (String::new(), tx.clone()));

    // Re-assemble the stream for handle_client. We already consumed the first
    // line, so we inject the username by pre-registering it in the client map
    // and calling handle_client_interactive directly.
    let result = handle_client_after_handshake(cid, username, reader, write_half, tx, &state).await;

    state.clients.lock().await.remove(&cid);
    result
}

/// Handle an interactive client after the handshake line has already been
/// consumed and parsed. This is the same logic as `handle_client` in mod.rs
/// but skips reading the first line since the daemon dispatcher already did.
async fn handle_client_after_handshake(
    cid: u64,
    username: &str,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    write_half: tokio::net::unix::OwnedWriteHalf,
    own_tx: broadcast::Sender<String>,
    state: &Arc<RoomState>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let username = username.to_owned();

    // Register username in the client map.
    {
        let mut map = state.clients.lock().await;
        if let Some(entry) = map.get_mut(&cid) {
            entry.0 = username.clone();
        }
    }

    // Register as host if no host has been set yet.
    {
        let mut host = state.host_user.lock().await;
        if host.is_none() {
            *host = Some(username.clone());
        }
    }

    eprintln!(
        "[daemon] {username} joined room {} (cid={cid})",
        state.room_id
    );

    // Track this user in the status map.
    state
        .status_map
        .lock()
        .await
        .insert(username.clone(), String::new());

    // Subscribe before sending history.
    let mut rx = own_tx.subscribe();

    // Send chat history, filtering DMs.
    let host_name = state.host_user.lock().await.clone();
    let is_host = host_name.as_deref() == Some(username.as_str());
    let history = crate::history::load(&state.chat_path)
        .await
        .unwrap_or_default();
    let mut write_half = write_half;
    for msg in &history {
        let visible = match msg {
            crate::message::Message::DirectMessage { user, to, .. } => {
                is_host || user == &username || to == &username
            }
            _ => true,
        };
        if visible {
            let line = format!("{}\n", serde_json::to_string(msg)?);
            if write_half.write_all(line.as_bytes()).await.is_err() {
                return Ok(());
            }
        }
    }

    // Broadcast join event.
    let join_msg = crate::message::make_join(&state.room_id, &username);
    if let Err(e) = super::fanout::broadcast_and_persist(
        &join_msg,
        &state.clients,
        &state.chat_path,
        &state.seq_counter,
    )
    .await
    {
        eprintln!("[daemon] broadcast_and_persist(join) failed: {e:#}");
        return Ok(());
    }

    // Wrap write half for shared use.
    let write_half = Arc::new(Mutex::new(write_half));

    // Outbound task.
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
                            eprintln!("[daemon] cid={cid} lagged by {n}");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = shutdown_rx.changed() => {
                    while let Ok(line) = rx.try_recv() {
                        let mut wh = write_half_out.lock().await;
                        let _ = wh.write_all(line.as_bytes()).await;
                    }
                    let _ = write_half_out.lock().await.shutdown().await;
                    break;
                }
            }
        }
    });

    // Inbound task.
    let username_in = username.clone();
    let room_id_in = state.room_id.clone();
    let write_half_in = write_half.clone();
    let state_in = state.clone();
    let inbound = tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;

        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match crate::message::parse_client_line(trimmed, &room_id_in, &username_in) {
                        Ok(msg) => {
                            match super::commands::route_command(msg, &username_in, &state_in).await
                            {
                                Ok(
                                    super::commands::CommandResult::Handled
                                    | super::commands::CommandResult::HandledWithReply(_),
                                ) => {}
                                Ok(super::commands::CommandResult::Reply(json)) => {
                                    let _ = write_half_in
                                        .lock()
                                        .await
                                        .write_all(format!("{json}\n").as_bytes())
                                        .await;
                                }
                                Ok(super::commands::CommandResult::Shutdown) => break,
                                Ok(super::commands::CommandResult::Passthrough(msg)) => {
                                    let result = match &msg {
                                        crate::message::Message::DirectMessage { to, .. } => {
                                            super::fanout::dm_and_persist(
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
                                            super::fanout::broadcast_and_persist(
                                                &msg,
                                                &state_in.clients,
                                                &state_in.chat_path,
                                                &state_in.seq_counter,
                                            )
                                            .await
                                        }
                                    };
                                    if let Err(e) = result {
                                        eprintln!("[daemon] persist error: {e:#}");
                                    }
                                }
                                Err(e) => eprintln!("[daemon] route error: {e:#}"),
                            }
                        }
                        Err(e) => {
                            eprintln!("[daemon] bad message from {username_in}: {e}");
                        }
                    }
                }
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
    let leave_msg = crate::message::make_leave(&state.room_id, &username);
    let _ = super::fanout::broadcast_and_persist(
        &leave_msg,
        &state.clients,
        &state.chat_path,
        &state.seq_counter,
    )
    .await;
    eprintln!(
        "[daemon] {username} left room {} (cid={cid})",
        state.room_id
    );

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_room_prefix ─────────────────────────────────────────────────

    #[test]
    fn parse_room_prefix_join() {
        let (room, rest) = parse_room_prefix("ROOM:myroom:JOIN:alice").unwrap();
        assert_eq!(room, "myroom");
        assert_eq!(rest, "JOIN:alice");
    }

    #[test]
    fn parse_room_prefix_token() {
        let (room, rest) = parse_room_prefix("ROOM:myroom:TOKEN:abc-123").unwrap();
        assert_eq!(room, "myroom");
        assert_eq!(rest, "TOKEN:abc-123");
    }

    #[test]
    fn parse_room_prefix_send() {
        let (room, rest) = parse_room_prefix("ROOM:myroom:SEND:bob").unwrap();
        assert_eq!(room, "myroom");
        assert_eq!(rest, "SEND:bob");
    }

    #[test]
    fn parse_room_prefix_interactive() {
        let (room, rest) = parse_room_prefix("ROOM:chat:alice").unwrap();
        assert_eq!(room, "chat");
        assert_eq!(rest, "alice");
    }

    #[test]
    fn parse_room_prefix_room_id_with_hyphens() {
        let (room, rest) = parse_room_prefix("ROOM:agent-room-2:JOIN:r2d2").unwrap();
        assert_eq!(room, "agent-room-2");
        assert_eq!(rest, "JOIN:r2d2");
    }

    #[test]
    fn parse_room_prefix_missing_prefix() {
        assert!(parse_room_prefix("JOIN:alice").is_none());
        assert!(parse_room_prefix("alice").is_none());
        assert!(parse_room_prefix("TOKEN:abc").is_none());
    }

    #[test]
    fn parse_room_prefix_empty_room_id() {
        assert!(parse_room_prefix("ROOM::JOIN:alice").is_none());
    }

    #[test]
    fn parse_room_prefix_no_rest() {
        // "ROOM:myroom:" — rest is empty string, valid (treated as empty username)
        let (room, rest) = parse_room_prefix("ROOM:myroom:").unwrap();
        assert_eq!(room, "myroom");
        assert_eq!(rest, "");
    }

    // ── DaemonState lifecycle ─────────────────────────────────────────────

    /// Test helper: look up a room's state by ID.
    async fn get_room(daemon: &DaemonState, room_id: &str) -> Arc<RoomState> {
        daemon
            .rooms
            .lock()
            .await
            .get(room_id)
            .cloned()
            .unwrap_or_else(|| panic!("room {room_id} not found"))
    }

    #[tokio::test]
    async fn create_room_succeeds() {
        let daemon = DaemonState::new(DaemonConfig::default());
        assert!(daemon.create_room("test-room").await.is_ok());
        let state = get_room(&daemon, "test-room").await;
        assert_eq!(*state.room_id, "test-room");
    }

    #[tokio::test]
    async fn create_duplicate_room_fails() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("dup").await.unwrap();
        let result = daemon.create_room("dup").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[tokio::test]
    async fn has_room_returns_true_for_created() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("room-a").await.unwrap();
        assert!(daemon.has_room("room-a").await);
        assert!(!daemon.has_room("room-b").await);
    }

    #[tokio::test]
    async fn destroy_room_removes_it() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("doomed").await.unwrap();
        assert!(daemon.destroy_room("doomed").await.is_ok());
        assert!(!daemon.has_room("doomed").await);
    }

    #[tokio::test]
    async fn destroy_nonexistent_room_fails() {
        let daemon = DaemonState::new(DaemonConfig::default());
        let result = daemon.destroy_room("nope").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn destroy_room_signals_shutdown() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("shutme").await.unwrap();
        let state = get_room(&daemon, "shutme").await;
        let rx = state.shutdown.subscribe();
        assert!(!*rx.borrow());

        daemon.destroy_room("shutme").await.unwrap();
        // The shutdown signal should now be true.
        assert!(*rx.borrow());
    }

    #[tokio::test]
    async fn list_rooms_returns_all() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("alpha").await.unwrap();
        daemon.create_room("beta").await.unwrap();
        daemon.create_room("gamma").await.unwrap();

        let mut rooms = daemon.list_rooms().await;
        rooms.sort();
        assert_eq!(rooms, vec!["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn list_rooms_empty_initially() {
        let daemon = DaemonState::new(DaemonConfig::default());
        assert!(daemon.list_rooms().await.is_empty());
    }

    #[tokio::test]
    async fn create_room_initializes_plugins() {
        let daemon = DaemonState::new(DaemonConfig::default());
        daemon.create_room("plugtest").await.unwrap();
        let state = get_room(&daemon, "plugtest").await;
        // help and stats should be registered
        assert!(state.plugin_registry.resolve("help").is_some());
        assert!(state.plugin_registry.resolve("stats").is_some());
    }

    // ── DaemonConfig ──────────────────────────────────────────────────────

    #[test]
    fn config_chat_path_format() {
        let config = DaemonConfig {
            data_dir: PathBuf::from("/var/room"),
            ..DaemonConfig::default()
        };
        assert_eq!(
            config.chat_path("myroom"),
            PathBuf::from("/var/room/myroom.chat")
        );
    }

    #[test]
    fn config_default_socket_path() {
        let config = DaemonConfig::default();
        assert_eq!(config.socket_path, PathBuf::from("/tmp/roomd.sock"));
    }
}
