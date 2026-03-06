use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        unix::{OwnedReadHalf, OwnedWriteHalf},
        UnixListener, UnixStream,
    },
    sync::{broadcast, Mutex, Notify},
};
use uuid::Uuid;

use crate::{
    history,
    message::{make_join, make_leave, make_system, parse_client_line, Message},
};

/// Maps client ID → (username, broadcast sender).
/// Username is set after the handshake completes.
type ClientMap = Arc<Mutex<HashMap<u64, (String, broadcast::Sender<String>)>>>;

/// Maps username → status string. Status is ephemeral; cleared on disconnect.
type StatusMap = Arc<Mutex<HashMap<String, String>>>;

/// The username of the first client to complete the handshake.
/// The host receives all DMs regardless of sender/recipient.
type HostUser = Arc<Mutex<Option<String>>>;

/// Maps token UUID → username. Populated by one-shot JOIN requests.
/// Cleared when the broker process exits; token files on disk survive restarts.
type TokenMap = Arc<Mutex<HashMap<String, String>>>;

/// Admin command names — routed through `handle_admin_cmd` when received as
/// a `Message::Command` with one of these cmd values.
const ADMIN_CMD_NAMES: &[&str] = &["kick", "reauth", "clear-tokens", "exit", "clear"];

/// Shared broker state passed to every client handler.
struct RoomState {
    clients: ClientMap,
    status_map: StatusMap,
    host_user: HostUser,
    token_map: TokenMap,
    chat_path: Arc<PathBuf>,
    room_id: Arc<String>,
    /// Signalled by the `/exit` admin command to shut down the broker run loop.
    shutdown: Arc<Notify>,
    /// Monotonically-increasing sequence counter. Incremented for every message
    /// broadcast or persisted by the broker, starting at 1.
    seq_counter: Arc<AtomicU64>,
}

pub struct Broker {
    room_id: String,
    chat_path: PathBuf,
    socket_path: PathBuf,
}

impl Broker {
    pub fn new(room_id: &str, chat_path: PathBuf, socket_path: PathBuf) -> Self {
        Self {
            room_id: room_id.to_owned(),
            chat_path,
            socket_path,
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

        let shutdown = Arc::new(Notify::new());
        let state = Arc::new(RoomState {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            token_map: Arc::new(Mutex::new(HashMap::new())),
            chat_path: Arc::new(self.chat_path.clone()),
            room_id: Arc::new(self.room_id.clone()),
            shutdown: shutdown.clone(),
            seq_counter: Arc::new(AtomicU64::new(0)),
        });
        let mut next_id: u64 = 0;

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    let (stream, _) = accept?;
                    next_id += 1;
                    let cid = next_id;

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
                _ = shutdown.notified() => {
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
        let username = token_map.lock().await.get(token).cloned();
        return match username {
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
    let shutdown_out = state.shutdown.clone();
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
                _ = shutdown_out.notified() => {
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
    let clients_in = clients.clone();
    let status_map_in = status_map.clone();
    let host_user_in = host_user.clone();
    let chat_path_in = chat_path.clone();
    let seq_counter_in = seq_counter.clone();
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
                        Ok(msg) => {
                            // Handle status commands privately (no broadcast of the Command itself)
                            if let Message::Command {
                                ref cmd,
                                ref params,
                                ..
                            } = msg
                            {
                                if cmd == "set_status" {
                                    let status = params.first().cloned().unwrap_or_default();
                                    status_map_in
                                        .lock()
                                        .await
                                        .insert(username_in.clone(), status.clone());
                                    let display = if status.is_empty() {
                                        format!("{username_in} cleared their status")
                                    } else {
                                        format!("{username_in} set status: {status}")
                                    };
                                    let sys = make_system(&room_id_in, "broker", display);
                                    if let Err(e) = broadcast_and_persist(
                                        &sys,
                                        &clients_in,
                                        &chat_path_in,
                                        &seq_counter_in,
                                    )
                                    .await
                                    {
                                        eprintln!("[broker] persist error: {e:#}");
                                    }
                                    continue;
                                } else if cmd == "who" {
                                    let map = status_map_in.lock().await;
                                    let mut entries: Vec<String> = map
                                        .iter()
                                        .map(|(u, s)| {
                                            if s.is_empty() {
                                                u.clone()
                                            } else {
                                                format!("{u}: {s}")
                                            }
                                        })
                                        .collect();
                                    entries.sort();
                                    drop(map);
                                    let content = if entries.is_empty() {
                                        "no users online".to_owned()
                                    } else {
                                        format!("online — {}", entries.join(", "))
                                    };
                                    let sys = make_system(&room_id_in, "broker", content);
                                    if let Ok(json) = serde_json::to_string(&sys) {
                                        let _ = write_half_in
                                            .lock()
                                            .await
                                            .write_all(format!("{json}\n").as_bytes())
                                            .await;
                                    }
                                    continue;
                                } else if ADMIN_CMD_NAMES.contains(&cmd.as_str()) {
                                    let cmd_line = format!("{cmd} {}", params.join(" "));
                                    if let Some(err) =
                                        handle_admin_cmd(&cmd_line, &username_in, &state_in).await
                                    {
                                        let sys = make_system(&room_id_in, "broker", err);
                                        if let Ok(json) = serde_json::to_string(&sys) {
                                            let _ = write_half_in
                                                .lock()
                                                .await
                                                .write_all(format!("{json}\n").as_bytes())
                                                .await;
                                        }
                                    }
                                    continue;
                                }
                            }
                            // Route DMs to sender + recipient + host only; broadcast everything else
                            let result = match &msg {
                                Message::DirectMessage { to, .. } => {
                                    dm_and_persist(
                                        &msg,
                                        &username_in,
                                        to,
                                        &host_user_in,
                                        &clients_in,
                                        &chat_path_in,
                                        &seq_counter_in,
                                    )
                                    .await
                                }
                                _ => {
                                    broadcast_and_persist(
                                        &msg,
                                        &clients_in,
                                        &chat_path_in,
                                        &seq_counter_in,
                                    )
                                    .await
                                }
                            };
                            if let Err(e) = result {
                                eprintln!("[broker] persist error: {e:#}");
                            }
                        }
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
    // Handle /who privately in oneshot context: return the user list without broadcasting.
    if let Message::Command {
        ref cmd,
        ref params,
        ..
    } = msg
    {
        if cmd == "who" {
            let map = state.status_map.lock().await;
            let mut entries: Vec<String> = map
                .iter()
                .map(|(u, s)| {
                    if s.is_empty() {
                        u.clone()
                    } else {
                        format!("{u}: {s}")
                    }
                })
                .collect();
            entries.sort();
            drop(map);
            let content = if entries.is_empty() {
                "no users online".to_owned()
            } else {
                format!("online — {}", entries.join(", "))
            };
            let sys = make_system(&state.room_id, "broker", content);
            let json = serde_json::to_string(&sys)?;
            write_half.write_all(format!("{json}\n").as_bytes()).await?;
            return Ok(());
        } else if ADMIN_CMD_NAMES.contains(&cmd.as_str()) {
            let cmd_line = format!("{cmd} {}", params.join(" "));
            let content = match handle_admin_cmd(&cmd_line, &username, state).await {
                None => "command executed".to_string(),
                Some(err) => err,
            };
            let reply = make_system(&state.room_id, "broker", content);
            let json = serde_json::to_string(&reply)?;
            write_half.write_all(format!("{json}\n").as_bytes()).await?;
            return Ok(());
        }
    }
    let result = match &msg {
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
            .await
        }
        _ => {
            broadcast_and_persist(&msg, &state.clients, &state.chat_path, &state.seq_counter).await
        }
    };
    let seq_msg = result?;
    let echo = format!("{}\n", serde_json::to_string(&seq_msg)?);
    write_half.write_all(echo.as_bytes()).await?;
    Ok(())
}

/// Handle a one-shot JOIN request: register a username, issue a UUID session token.
///
/// If the username is already registered the broker returns an error envelope and
/// closes the connection without issuing a token. The token is held in-memory for
/// the lifetime of the broker process; token files on disk are managed by the CLI.
async fn handle_oneshot_join(
    username: String,
    mut write_half: OwnedWriteHalf,
    token_map: &TokenMap,
) -> anyhow::Result<()> {
    let mut map = token_map.lock().await;
    if map.values().any(|u| u == &username) {
        let err = serde_json::json!({"type":"error","code":"username_taken","username": username});
        write_half.write_all(format!("{err}\n").as_bytes()).await?;
        return Ok(());
    }
    let token = Uuid::new_v4().to_string();
    map.insert(token.clone(), username.clone());
    drop(map);
    let resp = serde_json::json!({"type":"token","token": token,"username": username});
    write_half.write_all(format!("{resp}\n").as_bytes()).await?;
    Ok(())
}

/// Dispatch a `\command [arg]` line sent from a connected client.
///
/// Returns `None` on success or `Some(error_message)` if the command was rejected.
/// The caller is responsible for delivering any error message back to the issuer.
///
/// Only the room host (the first user to complete the interactive join handshake) is
/// authorised to run admin commands. All other callers receive a permission denied error.
///
/// Supported commands:
/// - `/kick <username>`      — invalidates the user's token so they cannot issue further
///   authenticated requests; the username remains reserved so they cannot rejoin without `\reauth`.
///   Also removes them from the status map so `/who` no longer shows them as online.
/// - `/reauth <username>`    — removes the user's token entirely so they can `room join` again.
/// - `/clear-tokens`         — removes every token for this room (all users must rejoin).
/// - `/exit`                 — broadcasts a shutdown notice then signals the broker to stop.
/// - `/clear`                — truncates the chat history file and broadcasts a notice.
async fn handle_admin_cmd(cmd_line: &str, issuer: &str, state: &RoomState) -> Option<String> {
    // Auth: only the room host may run admin commands.
    let host = state.host_user.lock().await.clone();
    if host.as_deref() != Some(issuer) {
        return Some(
            "permission denied: admin commands are restricted to the room host".to_string(),
        );
    }

    let room_id = state.room_id.as_str();
    let clients = &state.clients;
    let token_map = &state.token_map;
    let status_map = &state.status_map;
    let chat_path = &state.chat_path;
    let shutdown = &state.shutdown;
    let seq_counter = &state.seq_counter;
    let mut parts = cmd_line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
        "kick" => {
            if arg.is_empty() {
                return None;
            }
            let target = arg.to_owned();
            let mut map = token_map.lock().await;
            // Remove all existing tokens for this username, then insert a per-user sentinel
            // so the username stays reserved. Using KICKED:<username> as the key ensures
            // kicking multiple users does not overwrite each other's sentinel entries.
            map.retain(|_, u| u != &target);
            map.insert(format!("KICKED:{target}"), target.clone());
            drop(map);
            // Remove from status map immediately so /who no longer shows the kicked user.
            status_map.lock().await.remove(&target);
            let content = format!("{issuer} kicked {target} (token invalidated)");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        "reauth" => {
            if arg.is_empty() {
                return None;
            }
            let target = arg.to_owned();
            let mut map = token_map.lock().await;
            map.retain(|_, u| u != &target);
            drop(map);
            // Remove the on-disk token file so the user can join afresh.
            let prefix = format!("room-{room_id}-");
            let suffix = format!("-{target}.token");
            if let Ok(entries) = std::fs::read_dir("/tmp") {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let s = name.to_string_lossy();
                    if s.starts_with(&prefix) && s.ends_with(&suffix) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            let content = format!("{issuer} reauthed {target} (token cleared, can rejoin)");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        "clear-tokens" => {
            token_map.lock().await.clear();
            // Remove all on-disk token files for this room.
            let prefix = format!("room-{room_id}-");
            if let Ok(entries) = std::fs::read_dir("/tmp") {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let s = name.to_string_lossy();
                    if s.starts_with(&prefix) && s.ends_with(".token") {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            let content = format!("{issuer} cleared all tokens (all users must rejoin)");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        "exit" => {
            let content = format!("{issuer} is shutting down the room");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
            // Wake the main accept loop AND all outbound tasks simultaneously.
            shutdown.notify_waiters();
        }
        "clear" => {
            // Truncate the history file.
            if let Err(e) = std::fs::write(chat_path.as_ref(), "") {
                eprintln!("[broker] \\clear failed: {e}");
                return None;
            }
            let content = format!("{issuer} cleared chat history");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        _ => {
            eprintln!("[broker] unknown admin command from {issuer}: \\{cmd_line}");
        }
    }
    None
}

/// Assign the next sequence number, persist a message, and fan it out to all connected clients.
///
/// Returns the message with its `seq` field populated so callers can echo it to one-shot senders.
async fn broadcast_and_persist(
    msg: &Message,
    clients: &ClientMap,
    chat_path: &Path,
    seq_counter: &Arc<AtomicU64>,
) -> anyhow::Result<Message> {
    let seq = seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
    let mut msg = msg.clone();
    msg.set_seq(seq);

    history::append(chat_path, &msg).await?;

    let line = format!("{}\n", serde_json::to_string(&msg)?);
    let map = clients.lock().await;
    for (_, tx) in map.values() {
        let _ = tx.send(line.clone());
    }
    Ok(msg)
}

/// Assign the next sequence number, persist a DM, and deliver it only to the sender,
/// the recipient, and the host.
/// If the recipient is offline the message is still persisted and the sender
/// receives their own echo; no error is returned.
async fn dm_and_persist(
    msg: &Message,
    sender: &str,
    recipient: &str,
    host_user: &HostUser,
    clients: &ClientMap,
    chat_path: &Path,
    seq_counter: &Arc<AtomicU64>,
) -> anyhow::Result<Message> {
    let seq = seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
    let mut msg = msg.clone();
    msg.set_seq(seq);

    history::append(chat_path, &msg).await?;

    let line = format!("{}\n", serde_json::to_string(&msg)?);
    let host = host_user.lock().await;
    let host_name = host.as_deref();
    let map = clients.lock().await;
    for (username, tx) in map.values() {
        if username == sender || username == recipient || host_name == Some(username.as_str()) {
            let _ = tx.send(line.clone());
        }
    }
    Ok(msg)
}
