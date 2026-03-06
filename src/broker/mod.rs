pub(crate) mod auth;
pub(crate) mod commands;
pub(crate) mod fanout;
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
    message::{make_join, make_leave, parse_client_line, Message},
    plugin::{self, PluginRegistry},
};
use auth::{handle_oneshot_join, validate_token};
use commands::{route_command, CommandResult};
use fanout::{broadcast_and_persist, dm_and_persist};
use state::RoomState;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        unix::{OwnedReadHalf, OwnedWriteHalf},
        UnixListener, UnixStream,
    },
    sync::{broadcast, watch, Mutex},
};

pub struct Broker {
    room_id: String,
    chat_path: PathBuf,
    socket_path: PathBuf,
    ws_port: Option<u16>,
}

impl Broker {
    pub fn new(
        room_id: &str,
        chat_path: PathBuf,
        socket_path: PathBuf,
        ws_port: Option<u16>,
    ) -> Self {
        Self {
            room_id: room_id.to_owned(),
            chat_path,
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

        let mut registry = PluginRegistry::new();
        registry.register(Box::new(plugin::help::HelpPlugin))?;
        registry.register(Box::new(plugin::stats::StatsPlugin))?;

        let state = Arc::new(RoomState {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            token_map: Arc::new(Mutex::new(HashMap::new())),
            chat_path: Arc::new(self.chat_path.clone()),
            room_id: Arc::new(self.room_id.clone()),
            shutdown: Arc::new(shutdown_tx),
            seq_counter: Arc::new(AtomicU64::new(0)),
            plugin_registry: Arc::new(registry),
        });
        let next_client_id = Arc::new(AtomicU64::new(0));

        // Start WebSocket/REST server if a port was configured.
        if let Some(port) = self.ws_port {
            let ws_state = ws::WsAppState {
                room_state: state.clone(),
                next_client_id: next_client_id.clone(),
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
    // Clone the Arc fields up-front so spawned tasks can capture owned handles.
    let clients = state.clients.clone();
    let status_map = state.status_map.clone();
    let host_user = state.host_user.clone();
    let token_map = state.token_map.clone();
    let chat_path = state.chat_path.clone();
    let room_id = state.room_id.clone();
    let seq_counter = state.seq_counter.clone();

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // First line: username handshake, or one of the one-shot prefixes:
    //   SEND:<username>  — legacy one-shot send
    //   TOKEN:<uuid>     — token-authenticated one-shot send
    //   JOIN:<username>  — register username, receive a session token
    let mut first = String::new();
    reader.read_line(&mut first).await?;
    let first_line = first.trim();

    if let Some(send_user) = first_line.strip_prefix("SEND:") {
        return handle_oneshot_send(send_user.to_owned(), reader, write_half, state).await;
    }

    if let Some(token) = first_line.strip_prefix("TOKEN:") {
        return match validate_token(token, &token_map).await {
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

    if let Some(join_user) = first_line.strip_prefix("JOIN:") {
        return handle_oneshot_join(join_user.to_owned(), write_half, &token_map).await;
    }

    // Remaining path: full interactive join — first_line is the username.
    let username = first_line.to_owned();
    if username.is_empty() {
        return Ok(());
    }

    // Register username in the client map
    {
        let mut map = clients.lock().await;
        if let Some(entry) = map.get_mut(&cid) {
            entry.0 = username.clone();
        }
    }

    // Register as host if no host has been set yet (first to complete handshake)
    {
        let mut host = host_user.lock().await;
        if host.is_none() {
            *host = Some(username.clone());
        }
    }

    eprintln!("[broker] {username} joined (cid={cid})");

    // Track this user in the status map (empty status by default)
    status_map
        .lock()
        .await
        .insert(username.clone(), String::new());

    // Subscribe before sending history so we don't miss concurrent messages
    let mut rx = own_tx.subscribe();

    // Send chat history directly to this client's socket, filtering DMs the
    // client is not party to (sender, recipient, or host).
    // If the client disconnects mid-replay, treat it as a clean exit.
    let host_name = host_user.lock().await.clone();
    let is_host = host_name.as_deref() == Some(username.as_str());
    let history = history::load(&chat_path).await.unwrap_or_default();
    for msg in &history {
        let visible = match msg {
            Message::DirectMessage { user, to, .. } => {
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

    // Broadcast join event (also persists it)
    let join_msg = make_join(room_id.as_str(), &username);
    if let Err(e) = broadcast_and_persist(&join_msg, &clients, &chat_path, &seq_counter).await {
        eprintln!("[broker] broadcast_and_persist(join) failed: {e:#}");
        return Ok(());
    }

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
    let room_id_in = room_id.clone();
    let write_half_in = write_half.clone();
    let state_in = state.clone();
    let inbound = tokio::spawn(async move {
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
                    match parse_client_line(trimmed, &room_id_in, &username_in) {
                        Ok(msg) => match route_command(msg, &username_in, &state_in).await {
                            Ok(CommandResult::Handled) => {}
                            Ok(CommandResult::Reply(json)) => {
                                let _ = write_half_in
                                    .lock()
                                    .await
                                    .write_all(format!("{json}\n").as_bytes())
                                    .await;
                            }
                            Ok(CommandResult::Shutdown) => break,
                            Ok(CommandResult::Passthrough(msg)) => {
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
                                    eprintln!("[broker] persist error: {e:#}");
                                }
                            }
                            Err(e) => eprintln!("[broker] route error: {e:#}"),
                        },
                        Err(e) => eprintln!("[broker] bad message from {username_in}: {e}"),
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

    // Remove user from status map on disconnect
    status_map.lock().await.remove(&username);

    // Broadcast leave event
    let leave_msg = make_leave(room_id.as_str(), &username);
    let _ = broadcast_and_persist(&leave_msg, &clients, &chat_path, &seq_counter).await;
    eprintln!("[broker] {username} left (cid={cid})");

    Ok(())
}

/// Handle a one-shot SEND connection: read one message line, route it, echo it back, close.
/// The sender is never registered in ClientMap/StatusMap and generates no join/leave events.
/// DM envelopes are routed via `dm_and_persist`; all other messages are broadcast.
async fn handle_oneshot_send(
    username: String,
    mut reader: BufReader<OwnedReadHalf>,
    mut write_half: OwnedWriteHalf,
    state: &RoomState,
) -> anyhow::Result<()> {
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let msg = parse_client_line(trimmed, &state.room_id, &username)?;
    match route_command(msg, &username, state).await? {
        CommandResult::Handled | CommandResult::Shutdown => {}
        CommandResult::Reply(json) => {
            write_half.write_all(format!("{json}\n").as_bytes()).await?;
        }
        CommandResult::Passthrough(msg) => {
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
            let echo = format!("{}\n", serde_json::to_string(&seq_msg)?);
            write_half.write_all(echo.as_bytes()).await?;
        }
    }
    Ok(())
}
