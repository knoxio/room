pub(crate) mod admin;
pub(crate) mod auth;
pub(crate) mod commands;
pub mod daemon;
pub(crate) mod fanout;
pub(crate) mod handshake;
pub(crate) mod persistence;
pub(crate) mod service;
pub(crate) mod session;
pub(crate) mod state;
pub(crate) mod token_store;
pub(crate) mod ws;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use crate::plugin::PluginRegistry;
use auth::{handle_oneshot_join, validate_token};
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
        let persisted_tokens = token_store::load_token_map(&self.token_map_path);
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
/// paths. Delegates setup, message processing, and teardown to
/// [`session`](super::session) — this function only handles UDS-specific I/O
/// (reading lines, writing bytes, shutdown signaling).
pub(crate) async fn run_interactive_session(
    cid: u64,
    username: &str,
    reader: BufReader<OwnedReadHalf>,
    mut write_half: OwnedWriteHalf,
    own_tx: broadcast::Sender<String>,
    state: &Arc<RoomState>,
) -> anyhow::Result<()> {
    let username = username.to_owned();

    // Subscribe before setup so we don't miss concurrent messages.
    let mut rx = own_tx.subscribe();

    // Shared setup: register client, elect host, load history, broadcast join.
    let history_lines = match session::session_setup(cid, &username, state).await {
        Ok(lines) => lines,
        Err(e) => {
            eprintln!("[broker] session_setup failed: {e:#}");
            return Ok(());
        }
    };

    // Send history to client over UDS.
    for line in &history_lines {
        if write_half
            .write_all(format!("{line}\n").as_bytes())
            .await
            .is_err()
        {
            return Ok(());
        }
    }

    // Wrap write half in Arc<Mutex> for shared use by outbound and inbound tasks.
    let write_half = Arc::new(Mutex::new(write_half));

    // Outbound: receive from broadcast channel, forward to client socket.
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

    // Inbound: read lines from client socket, delegate to shared processing.
    let username_in = username.clone();
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
                    match session::process_inbound_message(trimmed, &username_in, &state_in).await {
                        session::InboundResult::Ok => {}
                        session::InboundResult::Reply(json) => {
                            let _ = write_half_in
                                .lock()
                                .await
                                .write_all(format!("{json}\n").as_bytes())
                                .await;
                        }
                        session::InboundResult::Shutdown => break,
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

    // Shared teardown: remove status, broadcast leave.
    session::session_teardown(cid, &username, state).await;

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
    let session::OneshotResult::Reply(reply) =
        session::process_oneshot_send(trimmed, &username, state).await?;
    write_half
        .write_all(format!("{reply}\n").as_bytes())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn read_line_limited_exact_limit_no_newline_accepted() {
        // Exactly MAX_LINE_BYTES of data with no trailing newline → EOF returns Ok.
        let data = vec![b'X'; MAX_LINE_BYTES];
        let cursor = std::io::Cursor::new(data);
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, MAX_LINE_BYTES);
        assert_eq!(buf.len(), MAX_LINE_BYTES);
    }

    #[tokio::test]
    async fn read_line_limited_just_over_limit_no_newline_rejected() {
        // MAX_LINE_BYTES + 1 bytes without newline → error before EOF.
        let data = vec![b'Y'; MAX_LINE_BYTES + 1];
        let cursor = std::io::Cursor::new(data);
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let result = read_line_limited(&mut reader, &mut buf).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds maximum"));
    }

    #[tokio::test]
    async fn read_line_limited_appends_to_existing_buffer() {
        // Buffer already has content — read_line_limited appends, does not overwrite.
        let data = b"world\n";
        let cursor = std::io::Cursor::new(data.to_vec());
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::from("hello ");
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, 6);
        assert_eq!(buf, "hello world\n");
    }

    #[tokio::test]
    async fn read_line_limited_embedded_null_bytes() {
        // Null bytes are valid UTF-8 — should be accepted.
        let data: Vec<u8> = vec![b'a', 0x00, b'b', b'\n'];
        let cursor = std::io::Cursor::new(data);
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, 4);
        assert_eq!(buf, "a\0b\n");
    }

    #[tokio::test]
    async fn read_line_limited_crlf_line_ending() {
        // CRLF: the \r is part of the line content, \n terminates.
        let data = b"line\r\n";
        let cursor = std::io::Cursor::new(data.to_vec());
        let mut reader = tokio::io::BufReader::new(cursor);
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, 6);
        assert_eq!(buf, "line\r\n");
    }

    #[tokio::test]
    async fn read_line_limited_long_line_with_newline_at_boundary() {
        // Line of MAX_LINE_BYTES - 1 chars + newline = exactly at limit.
        let mut data = vec![b'Z'; MAX_LINE_BYTES - 1];
        data.push(b'\n');
        // Add trailing data to verify only one line is consumed.
        data.extend_from_slice(b"next\n");
        let cursor = std::io::Cursor::new(data);
        let mut reader = tokio::io::BufReader::new(cursor);

        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n, MAX_LINE_BYTES);
        assert!(buf.ends_with('\n'));
        assert_eq!(buf.len(), MAX_LINE_BYTES);

        // Second line should be readable independently.
        buf.clear();
        let n2 = read_line_limited(&mut reader, &mut buf).await.unwrap();
        assert_eq!(n2, 5);
        assert_eq!(buf, "next\n");
    }
}
