use std::path::Path;

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::message::Message;

// ── SocketTarget ──────────────────────────────────────────────────────────────

/// Resolved connection target for a broker.
///
/// When `daemon_room` is `Some(room_id)`, the client is connecting to the
/// multi-room daemon (`roomd`) and must prepend `ROOM:<room_id>:` before every
/// handshake token so the daemon can route the connection to the correct room.
///
/// When `daemon_room` is `None`, the client is connecting to a single-room
/// broker socket and sends handshake tokens directly (e.g. `TOKEN:<uuid>`).
#[derive(Debug, Clone)]
pub struct SocketTarget {
    /// Path to the UDS socket.
    pub path: std::path::PathBuf,
    /// If `Some(room_id)`, prepend `ROOM:<room_id>:` before each handshake token.
    pub daemon_room: Option<String>,
}

impl SocketTarget {
    /// Construct the full first line to send for a given handshake token.
    ///
    /// - Per-room: `TOKEN:<uuid>` → `"TOKEN:<uuid>"`
    /// - Daemon: `TOKEN:<uuid>` → `"ROOM:<room_id>:TOKEN:<uuid>"`
    fn handshake_line(&self, token_line: &str) -> String {
        match &self.daemon_room {
            Some(room_id) => format!("ROOM:{room_id}:{token_line}"),
            None => token_line.to_owned(),
        }
    }
}

// ── Socket target resolution ──────────────────────────────────────────────────

/// Resolve the effective socket target for a given room.
///
/// Resolution order:
/// 1. If `explicit` is given, use it. If the path is not the per-room socket
///    for this room, assume it is a daemon socket and use the `ROOM:` prefix.
/// 2. Otherwise (auto-discovery): try the platform-native daemon socket first
///    (`room_socket_path()`); fall back to the per-room socket if the daemon
///    socket does not exist.
pub fn resolve_socket_target(room_id: &str, explicit: Option<&Path>) -> SocketTarget {
    let per_room = crate::paths::room_single_socket_path(room_id);
    // ROOM_SOCKET env var overrides the default daemon socket path.
    let daemon = crate::paths::effective_socket_path(None);

    if let Some(path) = explicit {
        // If the caller gave us the per-room socket path, use per-room mode.
        // Any other explicit path is treated as a daemon socket.
        if path == per_room {
            return SocketTarget {
                path: path.to_owned(),
                daemon_room: None,
            };
        }
        return SocketTarget {
            path: path.to_owned(),
            daemon_room: Some(room_id.to_owned()),
        };
    }

    // Auto-discovery: prefer daemon if it is running.
    if daemon.exists() {
        SocketTarget {
            path: daemon,
            daemon_room: Some(room_id.to_owned()),
        }
    } else {
        SocketTarget {
            path: per_room,
            daemon_room: None,
        }
    }
}

// ── Daemon auto-start ─────────────────────────────────────────────────────────

const DAEMON_POLL_INTERVAL_MS: u64 = 50;
const DAEMON_START_TIMEOUT_MS: u64 = 5_000;

/// Ensure the multi-room daemon is running.
///
/// If the daemon socket is not connectable, spawns `room daemon` as a detached
/// background process, writes its PID to `~/.room/roomd.pid`, and polls until
/// the socket accepts connections (up to 15 seconds).
///
/// This is a no-op when the caller passes an explicit `--socket` override — in
/// that case the caller is targeting a specific socket and the daemon should not
/// be auto-started on their behalf.
///
/// # Errors
///
/// Returns an error if the process cannot be spawned or if the socket does not
/// become connectable within the timeout.
pub async fn ensure_daemon_running() -> anyhow::Result<()> {
    let exe = resolve_daemon_binary()?;
    // Respect ROOM_SOCKET env var when deciding where to start/find the daemon.
    ensure_daemon_running_impl(&crate::paths::effective_socket_path(None), &exe).await
}

/// Resolve which binary to spawn as the daemon.
///
/// Resolution order:
/// 1. `ROOM_BINARY` env var (explicit override for testing).
/// 2. `which room` — the installed binary on `$PATH`.
/// 3. `current_exe()` — fallback to the running binary.
///
/// Using the installed binary (not `current_exe()`) ensures all agents
/// converge on a single shared daemon regardless of which git worktree
/// they run from.
fn resolve_daemon_binary() -> anyhow::Result<std::path::PathBuf> {
    // 1. Explicit override.
    if let Ok(p) = std::env::var("ROOM_BINARY") {
        let path = std::path::PathBuf::from(&p);
        if path.exists() {
            return Ok(path);
        }
    }

    // 2. Installed binary on PATH.
    if let Ok(output) = std::process::Command::new("which").arg("room").output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout);
            let path = std::path::PathBuf::from(path_str.trim());
            if path.exists() {
                return Ok(path);
            }
        }
    }

    // 3. Fallback to current executable.
    std::env::current_exe().map_err(|e| anyhow::anyhow!("cannot resolve daemon binary: {e}"))
}

/// Test-visible variant: accepts explicit socket and exe paths so tests can
/// target temp paths without relying on `current_exe()`.
#[cfg(test)]
pub(crate) async fn ensure_daemon_running_at(
    socket: &Path,
    exe: &std::path::Path,
) -> anyhow::Result<()> {
    ensure_daemon_running_impl(socket, exe).await
}

async fn ensure_daemon_running_impl(socket: &Path, exe: &Path) -> anyhow::Result<()> {
    // Fast path: daemon is already running.
    if UnixStream::connect(socket).await.is_ok() {
        return Ok(());
    }

    let child = std::process::Command::new(exe)
        .arg("daemon")
        .arg("--socket")
        .arg(socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn daemon ({}): {e}", exe.display()))?;

    // Persist PID so the user (or cleanup scripts) can identify the process.
    let pid_path = crate::paths::room_pid_path();
    let _ = std::fs::write(&pid_path, child.id().to_string());

    // Poll until the socket accepts connections.
    let deadline =
        tokio::time::Instant::now() + tokio::time::Duration::from_millis(DAEMON_START_TIMEOUT_MS);

    loop {
        if UnixStream::connect(socket).await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "daemon failed to start within {}ms (socket: {})",
                DAEMON_START_TIMEOUT_MS,
                socket.display()
            );
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(DAEMON_POLL_INTERVAL_MS)).await;
    }
}

// ── Transport functions ───────────────────────────────────────────────────────

/// Connect to a running broker and deliver a single message without joining the room.
/// Returns the broadcast echo (with broker-assigned id/ts) so callers have the message ID.
///
/// # Deprecation
///
/// Uses the `SEND:<username>` handshake which bypasses token authentication.
/// Use [`send_message_with_token`] instead — obtain a token via `room join` first.
#[deprecated(
    since = "3.1.0",
    note = "SEND: handshake is unauthenticated; use send_message_with_token instead"
)]
pub async fn send_message(
    socket_path: &Path,
    username: &str,
    content: &str,
) -> anyhow::Result<Message> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to broker at {}: {e}", socket_path.display())
    })?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("SEND:{username}\n").as_bytes()).await?;
    w.write_all(format!("{content}\n").as_bytes()).await?;

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let msg: Message = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("broker returned invalid JSON: {e}: {:?}", line.trim()))?;
    Ok(msg)
}

/// Connect to a running broker and deliver a single message authenticated by token.
///
/// When `target.daemon_room` is `Some(room_id)`, sends
/// `ROOM:<room_id>:TOKEN:<token>` as the handshake so the daemon routes
/// the connection to the correct room. For a per-room socket the handshake
/// is simply `TOKEN:<token>`.
pub async fn send_message_with_token(
    socket_path: &Path,
    token: &str,
    content: &str,
) -> anyhow::Result<Message> {
    send_message_with_token_target(
        &SocketTarget {
            path: socket_path.to_owned(),
            daemon_room: None,
        },
        token,
        content,
    )
    .await
}

/// Variant of [`send_message_with_token`] that takes a fully-resolved
/// [`SocketTarget`], including daemon routing prefix when required.
pub async fn send_message_with_token_target(
    target: &SocketTarget,
    token: &str,
    content: &str,
) -> anyhow::Result<Message> {
    let stream = UnixStream::connect(&target.path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to broker at {}: {e}", target.path.display())
    })?;
    let (r, mut w) = stream.into_split();
    let handshake = target.handshake_line(&format!("TOKEN:{token}"));
    w.write_all(format!("{handshake}\n").as_bytes()).await?;
    // content is already a JSON envelope from cmd_send; newlines are escaped by serde.
    w.write_all(format!("{content}\n").as_bytes()).await?;

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    // Broker may return an error envelope instead of a broadcast echo.
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("broker returned invalid JSON: {e}: {:?}", line.trim()))?;
    if v["type"] == "error" {
        let code = v["code"].as_str().unwrap_or("unknown");
        if code == "invalid_token" {
            anyhow::bail!("invalid token — run: room join {}", target.path.display());
        }
        anyhow::bail!("broker error: {code}");
    }
    let msg: Message = serde_json::from_value(v)
        .map_err(|e| anyhow::anyhow!("broker returned unexpected JSON: {e}"))?;
    Ok(msg)
}

/// Register a username with the broker and obtain a session token.
///
/// The broker checks for username collisions. On success it returns a token
/// envelope; on collision it returns an error envelope.
pub async fn join_session(socket_path: &Path, username: &str) -> anyhow::Result<(String, String)> {
    join_session_target(
        &SocketTarget {
            path: socket_path.to_owned(),
            daemon_room: None,
        },
        username,
    )
    .await
}

/// Variant of [`join_session`] that takes a fully-resolved [`SocketTarget`].
pub async fn join_session_target(
    target: &SocketTarget,
    username: &str,
) -> anyhow::Result<(String, String)> {
    let stream = UnixStream::connect(&target.path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to broker at {}: {e}", target.path.display())
    })?;
    let (r, mut w) = stream.into_split();
    let handshake = target.handshake_line(&format!("JOIN:{username}"));
    w.write_all(format!("{handshake}\n").as_bytes()).await?;

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("broker returned invalid JSON: {e}: {:?}", line.trim()))?;
    if v["type"] == "error" {
        let code = v["code"].as_str().unwrap_or("unknown");
        if code == "username_taken" {
            anyhow::bail!("username '{}' is already in use in this room", username);
        }
        anyhow::bail!("broker error: {code}");
    }
    let token = v["token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("broker response missing 'token' field"))?
        .to_owned();
    let returned_user = v["username"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("broker response missing 'username' field"))?
        .to_owned();
    Ok((returned_user, token))
}

/// Global user registration: sends `JOIN:<username>` directly to the daemon.
///
/// Unlike [`join_session`] which routes through `ROOM:<room_id>:JOIN:<username>`,
/// this sends `JOIN:<username>` at daemon level — no room association.
/// Returns the existing token if the username is already registered.
pub async fn global_join_session(
    socket_path: &Path,
    username: &str,
) -> anyhow::Result<(String, String)> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to daemon at {}: {e}", socket_path.display())
    })?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("JOIN:{username}\n").as_bytes()).await?;

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("daemon returned invalid JSON: {e}: {:?}", line.trim()))?;
    if v["type"] == "error" {
        let code = v["code"].as_str().unwrap_or("unknown");
        anyhow::bail!("daemon error: {code}");
    }
    let token = v["token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("daemon response missing 'token' field"))?
        .to_owned();
    let returned_user = v["username"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("daemon response missing 'username' field"))?
        .to_owned();
    Ok((returned_user, token))
}

// ── Room creation ────────────────────────────────────────────────────────────

/// Connect to a daemon socket and create a new room via `CREATE:<room_id>`.
///
/// Sends the room ID on the first line and the config JSON on the second.
/// Returns the daemon's response JSON on success (`{"type":"room_created",...}`).
pub async fn create_room(
    socket_path: &Path,
    room_id: &str,
    config_json: &str,
) -> anyhow::Result<serde_json::Value> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to daemon at {}: {e}", socket_path.display())
    })?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("CREATE:{room_id}\n").as_bytes())
        .await?;
    w.write_all(format!("{config_json}\n").as_bytes()).await?;

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("daemon returned invalid JSON: {e}: {:?}", line.trim()))?;
    if v["type"] == "error" {
        let message = v["message"].as_str().unwrap_or("unknown error");
        anyhow::bail!("{message}");
    }
    Ok(v)
}

// ── Room destruction ─────────────────────────────────────────────────────────

/// Connect to a daemon socket and destroy a room via `DESTROY:<room_id>`.
///
/// Returns the daemon's response JSON on success (`{"type":"room_destroyed",...}`).
pub async fn destroy_room(socket_path: &Path, room_id: &str) -> anyhow::Result<serde_json::Value> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to daemon at {}: {e}", socket_path.display())
    })?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("DESTROY:{room_id}\n").as_bytes())
        .await?;

    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("daemon returned invalid JSON: {e}: {:?}", line.trim()))?;
    if v["type"] == "error" {
        let message = v["message"]
            .as_str()
            .unwrap_or(v["code"].as_str().unwrap_or("unknown error"));
        anyhow::bail!("{message}");
    }
    Ok(v)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn per_room_target(room_id: &str) -> SocketTarget {
        SocketTarget {
            path: PathBuf::from(format!("/tmp/room-{room_id}.sock")),
            daemon_room: None,
        }
    }

    fn daemon_target(room_id: &str) -> SocketTarget {
        SocketTarget {
            path: PathBuf::from("/tmp/roomd.sock"),
            daemon_room: Some(room_id.to_owned()),
        }
    }

    // ── SocketTarget::handshake_line ──────────────────────────────────────────

    #[test]
    fn per_room_token_handshake_no_prefix() {
        let t = per_room_target("myroom");
        assert_eq!(t.handshake_line("TOKEN:abc-123"), "TOKEN:abc-123");
    }

    #[test]
    fn daemon_token_handshake_has_room_prefix() {
        let t = daemon_target("myroom");
        assert_eq!(
            t.handshake_line("TOKEN:abc-123"),
            "ROOM:myroom:TOKEN:abc-123"
        );
    }

    #[test]
    fn per_room_join_handshake_no_prefix() {
        let t = per_room_target("chat");
        assert_eq!(t.handshake_line("JOIN:alice"), "JOIN:alice");
    }

    #[test]
    fn daemon_join_handshake_has_room_prefix() {
        let t = daemon_target("chat");
        assert_eq!(t.handshake_line("JOIN:alice"), "ROOM:chat:JOIN:alice");
    }

    #[test]
    fn daemon_handshake_with_hyphen_room_id() {
        let t = daemon_target("agent-room-2");
        assert_eq!(
            t.handshake_line("TOKEN:uuid"),
            "ROOM:agent-room-2:TOKEN:uuid"
        );
    }

    // ── ensure_daemon_running_at ──────────────────────────────────────────────
    //
    // WARNING: these two tests spawn real `room` daemon processes.
    // They are marked #[ignore] so they don't run in normal `cargo test`.
    // Run explicitly with: `cargo test -p room-cli -- --ignored ensure_daemon`

    /// Resolve the `room` binary from the test binary's location.
    /// In cargo test layout: `target/debug/deps/../room` → `target/debug/room`.
    fn room_bin() -> PathBuf {
        let bin = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("room");
        assert!(bin.exists(), "room binary not found at {}", bin.display());
        bin
    }

    /// Verify that `ensure_daemon_running_at` is a no-op when a live socket already exists.
    /// Ignored by default — spawns a real daemon process. Run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "spawns a real daemon process; run explicitly with `cargo test -- --ignored`"]
    async fn ensure_daemon_noop_when_socket_connectable() {
        // Start a real daemon at a temp socket, verify ensure_daemon_running_at
        // returns Ok without spawning a second process.
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("roomd.sock");
        let exe = room_bin();

        let mut child = tokio::process::Command::new(&exe)
            .args(["daemon", "--socket"])
            .arg(&socket)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn room daemon");

        // Wait for socket to become connectable (up to 10s under parallel load).
        for _ in 0..200 {
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
        assert!(
            tokio::net::UnixStream::connect(&socket).await.is_ok(),
            "daemon socket not ready"
        );

        // Calling again must be a no-op (does not error).
        ensure_daemon_running_at(&socket, &exe).await.unwrap();

        child.kill().await.ok();
    }

    /// Verify that `ensure_daemon_running_at` auto-starts a daemon when none is running.
    /// Ignored by default — spawns a real daemon process. Run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "spawns a real daemon process; run explicitly with `cargo test -- --ignored`"]
    async fn ensure_daemon_starts_daemon_and_writes_pid() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("autostart.sock");
        let exe = room_bin();

        // No daemon running yet — ensure_daemon_running_at must start one.
        ensure_daemon_running_at(&socket, &exe).await.unwrap();

        // Socket should now be connectable.
        assert!(
            tokio::net::UnixStream::connect(&socket).await.is_ok(),
            "daemon socket not connectable after auto-start"
        );
        // Note: PID file is written to the global room_pid_path() which other
        // parallel tests may also write/clean up. Asserting its contents here
        // would be racy. Verifying socket connectivity is sufficient.
        //
        // TempDir drop cleans up the socket; the daemon will exit when it can
        // no longer accept connections.
    }

    // ── resolve_socket_target ─────────────────────────────────────────────────

    #[test]
    fn resolve_explicit_per_room_socket_is_not_daemon() {
        let per_room = crate::paths::room_single_socket_path("myroom");
        let target = resolve_socket_target("myroom", Some(&per_room));
        assert_eq!(target.path, per_room);
        assert!(
            target.daemon_room.is_none(),
            "per-room socket should not set daemon_room"
        );
    }

    #[test]
    fn resolve_explicit_daemon_socket_is_daemon() {
        let daemon_sock = PathBuf::from("/tmp/roomd.sock");
        let target = resolve_socket_target("myroom", Some(&daemon_sock));
        assert_eq!(target.path, daemon_sock);
        assert_eq!(target.daemon_room.as_deref(), Some("myroom"));
    }

    #[test]
    fn resolve_explicit_custom_path_is_daemon() {
        let custom = PathBuf::from("/var/run/roomd-test.sock");
        let target = resolve_socket_target("chat", Some(&custom));
        assert_eq!(target.path, custom);
        assert_eq!(target.daemon_room.as_deref(), Some("chat"));
    }

    #[test]
    fn resolve_auto_no_daemon_falls_back_to_per_room() {
        // When no daemon socket exists (we check the real daemon path, which
        // is unlikely to exist during CI), auto-discovery should fall back.
        // We can only test this if the daemon socket is NOT running.
        let daemon_path = crate::paths::room_socket_path();
        if !daemon_path.exists() {
            let target = resolve_socket_target("myroom", None);
            assert_eq!(target.path, crate::paths::room_single_socket_path("myroom"));
            assert!(target.daemon_room.is_none());
        }
        // If daemon IS running, skip (we can't test both branches in one call).
    }

    // ── resolve_daemon_binary ────────────────────────────────────────────────

    /// Env var access is process-global; serialize tests that mutate it.
    static TRANSPORT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_daemon_binary_uses_room_binary_env() {
        let _lock = TRANSPORT_ENV_LOCK.lock().unwrap();
        let key = "ROOM_BINARY";
        let prev = std::env::var(key).ok();

        // Point ROOM_BINARY at a real binary that exists.
        let target = std::env::current_exe().unwrap();
        std::env::set_var(key, &target);
        let result = resolve_daemon_binary().unwrap();
        assert_eq!(result, target, "should use ROOM_BINARY when set");

        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn resolve_daemon_binary_ignores_nonexistent_room_binary() {
        let _lock = TRANSPORT_ENV_LOCK.lock().unwrap();
        let key = "ROOM_BINARY";
        let prev = std::env::var(key).ok();

        std::env::set_var(key, "/nonexistent/path/to/room");
        let result = resolve_daemon_binary().unwrap();
        // Should NOT be the nonexistent path — falls through to which/current_exe.
        assert_ne!(
            result,
            std::path::PathBuf::from("/nonexistent/path/to/room"),
            "should skip ROOM_BINARY when path does not exist"
        );

        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn resolve_daemon_binary_falls_back_without_env() {
        let _lock = TRANSPORT_ENV_LOCK.lock().unwrap();
        let key = "ROOM_BINARY";
        let prev = std::env::var(key).ok();

        std::env::remove_var(key);
        let result = resolve_daemon_binary().unwrap();
        // Should resolve to either `which room` or current_exe — either way a real path.
        assert!(result.exists(), "resolved binary should exist: {result:?}");

        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
