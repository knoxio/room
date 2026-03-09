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
    let daemon = crate::paths::room_socket_path();

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

// ── Transport functions ───────────────────────────────────────────────────────

/// Connect to a running broker and deliver a single message without joining the room.
/// Returns the broadcast echo (with broker-assigned id/ts) so callers have the message ID.
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
}
