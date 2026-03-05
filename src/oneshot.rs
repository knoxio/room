use std::path::{Path, PathBuf};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::{history, message::Message};

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
/// Sends `TOKEN:<token>\n` as the handshake (instead of `SEND:<username>\n`).
/// The broker resolves the username from its in-memory token map.
pub async fn send_message_with_token(
    socket_path: &Path,
    token: &str,
    content: &str,
) -> anyhow::Result<Message> {
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to broker at {}: {e}", socket_path.display())
    })?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("TOKEN:{token}\n").as_bytes()).await?;
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
            anyhow::bail!("invalid token — run: room join {}", socket_path.display());
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
    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!("cannot connect to broker at {}: {e}", socket_path.display())
    })?;
    let (r, mut w) = stream.into_split();
    w.write_all(format!("JOIN:{username}\n").as_bytes()).await?;

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

/// Returns the canonical token file path: `/tmp/room-<room_id>-<username>.token`.
///
/// One file per (room, user) pair — multiple agents on the same machine never
/// overwrite each other's tokens.
pub fn token_file_path(room_id: &str, username: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/room-{room_id}-{username}.token"))
}

/// One-shot join subcommand: register username, receive token, write token file.
///
/// Writes to `/tmp/room-<room_id>-<username>.token` so agents sharing a machine
/// do not clobber each other. Subsequent `send`, `poll`, and `watch` calls find
/// the file automatically (single-agent) or via `--user <username>` (multi-agent).
pub async fn cmd_join(room_id: &str, username: &str) -> anyhow::Result<()> {
    let socket_path = PathBuf::from(format!("/tmp/room-{room_id}.sock"));
    let (returned_user, token) = join_session(&socket_path, username).await?;
    let token_data = serde_json::json!({"username": returned_user, "token": token});
    let token_path = token_file_path(room_id, &returned_user);
    std::fs::write(&token_path, format!("{token_data}\n"))?;
    println!("{token_data}");
    Ok(())
}

/// Look up the username associated with `token` by scanning stored token files for `room_id`.
///
/// `room join` writes `/tmp/room-<room_id>-<username>.token` for each session.
/// This function finds the file whose `token` field matches the given value and
/// returns the corresponding username. Used by `poll` and `watch` to resolve the
/// cursor file path without requiring the caller to pass a username explicitly.
pub fn username_from_token(room_id: &str, token: &str) -> anyhow::Result<String> {
    let prefix = format!("room-{room_id}-");
    let suffix = ".token";
    let files: Vec<PathBuf> = std::fs::read_dir("/tmp")
        .map_err(|e| anyhow::anyhow!("cannot read /tmp: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&prefix) && n.ends_with(suffix))
                .unwrap_or(false)
        })
        .collect();

    for path in files {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                if v["token"].as_str() == Some(token) {
                    if let Some(u) = v["username"].as_str() {
                        return Ok(u.to_owned());
                    }
                }
            }
        }
    }

    anyhow::bail!("token not recognised — run: room join {room_id} <username> to get a fresh token")
}

/// One-shot send subcommand: connect, send, print echo JSON to stdout, exit.
///
/// Authenticates via `token` (from `room join`). The broker resolves the sender's
/// username from the token — no username arg required. When `to` is `Some(recipient)`,
/// the message is sent as a DM routed only to sender, recipient, and host.
pub async fn cmd_send(
    room_id: &str,
    token: &str,
    to: Option<&str>,
    content: &str,
) -> anyhow::Result<()> {
    let socket_path = PathBuf::from(format!("/tmp/room-{room_id}.sock"));
    let wire = match to {
        Some(recipient) => {
            serde_json::json!({"type": "dm", "to": recipient, "content": content}).to_string()
        }
        None => serde_json::json!({"type": "message", "content": content}).to_string(),
    };
    let msg = send_message_with_token(&socket_path, token, &wire)
        .await
        .map_err(|e| {
            if e.to_string().contains("invalid token") {
                anyhow::anyhow!("invalid token — run: room join {room_id} <username>")
            } else {
                e
            }
        })?;
    println!("{}", serde_json::to_string(&msg)?);
    Ok(())
}

/// Return all messages from `chat_path` after the message with ID `since` (exclusive).
///
/// If `since` is `None`, the cursor file at `cursor_path` is checked for a previously
/// stored position. A `None` cursor means all messages are returned.
///
/// `viewer` is the username of the caller. When `Some`, `DirectMessage` entries are
/// filtered to only those where the viewer is the sender or the recipient. Pass `None`
/// to skip DM filtering (e.g. in tests that don't involve DMs).
///
/// The cursor file is updated to the last returned message's ID after each successful call.
pub async fn poll_messages(
    chat_path: &Path,
    cursor_path: &Path,
    viewer: Option<&str>,
    since: Option<&str>,
) -> anyhow::Result<Vec<Message>> {
    let effective_since: Option<String> = since
        .map(|s| s.to_owned())
        .or_else(|| read_cursor(cursor_path));

    let messages = history::load(chat_path).await?;

    let start = match &effective_since {
        Some(id) => messages
            .iter()
            .position(|m| m.id() == id)
            .map(|i| i + 1)
            .unwrap_or(0),
        None => 0,
    };

    let result: Vec<Message> = messages[start..]
        .iter()
        .filter(|m| match m {
            Message::DirectMessage { user, to, .. } => viewer
                .map(|v| v == user.as_str() || v == to.as_str())
                .unwrap_or(true),
            _ => true,
        })
        .cloned()
        .collect();

    if let Some(last) = result.last() {
        write_cursor(cursor_path, last.id())?;
    }

    Ok(result)
}

/// Watch subcommand: poll in a loop until at least one foreign `Message` arrives.
///
/// Reads the caller's username from the session token file. Polls every
/// `interval_secs` seconds, filtering out own messages and non-`Message` variants.
/// Exits after printing the first batch of foreign messages as NDJSON.
/// Shares the cursor file with `room poll` — the two subcommands never re-deliver
/// the same message.
pub async fn cmd_watch(room_id: &str, token: &str, interval_secs: u64) -> anyhow::Result<()> {
    let username = username_from_token(room_id, token)?;
    let meta_path = PathBuf::from(format!("/tmp/room-{room_id}.meta"));
    let chat_path = chat_path_from_meta(room_id, &meta_path);
    let cursor_path = PathBuf::from(format!("/tmp/room-{room_id}-{username}.cursor"));

    loop {
        let messages = poll_messages(&chat_path, &cursor_path, Some(&username), None).await?;

        let foreign: Vec<&Message> = messages
            .iter()
            .filter(|m| matches!(m, Message::Message { user, .. } if user != &username))
            .collect();

        if !foreign.is_empty() {
            for msg in foreign {
                println!("{}", serde_json::to_string(msg)?);
            }
            return Ok(());
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(interval_secs)).await;
    }
}

/// One-shot poll subcommand: read messages since cursor, print as NDJSON, update cursor.
///
/// Reads the caller's username from the session token file.
pub async fn cmd_poll(room_id: &str, token: &str, since: Option<String>) -> anyhow::Result<()> {
    let username = username_from_token(room_id, token)?;
    let meta_path = PathBuf::from(format!("/tmp/room-{room_id}.meta"));
    let chat_path = chat_path_from_meta(room_id, &meta_path);
    let cursor_path = PathBuf::from(format!("/tmp/room-{room_id}-{username}.cursor"));

    let messages =
        poll_messages(&chat_path, &cursor_path, Some(&username), since.as_deref()).await?;
    for msg in &messages {
        println!("{}", serde_json::to_string(msg)?);
    }
    Ok(())
}

fn chat_path_from_meta(room_id: &str, meta_path: &Path) -> PathBuf {
    if meta_path.exists() {
        if let Ok(data) = std::fs::read_to_string(meta_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(p) = v["chat_path"].as_str() {
                    return PathBuf::from(p);
                }
            }
        }
    }
    history::default_chat_path(room_id)
}

fn read_cursor(cursor_path: &Path) -> Option<String> {
    std::fs::read_to_string(cursor_path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn write_cursor(cursor_path: &Path, id: &str) -> anyhow::Result<()> {
    std::fs::write(cursor_path, id)?;
    Ok(())
}

#[cfg(test)]
mod token_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write a token file into a temp dir and return the token string.
    fn write_token_file(dir: &std::path::Path, room_id: &str, username: &str, token: &str) {
        let name = format!("room-{room_id}-{username}.token");
        let data = serde_json::json!({"username": username, "token": token});
        fs::write(dir.join(name), format!("{data}\n")).unwrap();
    }

    /// A version of username_from_token that scans a custom directory (for hermetic tests).
    fn username_from_token_in(
        dir: &std::path::Path,
        room_id: &str,
        token: &str,
    ) -> anyhow::Result<String> {
        let prefix = format!("room-{room_id}-");
        let suffix = ".token";
        let files: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(&prefix) && n.ends_with(suffix))
                    .unwrap_or(false)
            })
            .collect();

        for path in files {
            if let Ok(data) = fs::read_to_string(&path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                    if v["token"].as_str() == Some(token) {
                        if let Some(u) = v["username"].as_str() {
                            return Ok(u.to_owned());
                        }
                    }
                }
            }
        }
        anyhow::bail!("token not recognised — run: room join {room_id} <username>")
    }

    /// token_file_path produces a per-user path that differs between users.
    #[test]
    fn token_file_path_is_per_user() {
        let alice = token_file_path("myroom", "alice");
        let bob = token_file_path("myroom", "bob");
        assert_ne!(alice, bob);
        assert!(alice.to_str().unwrap().contains("alice"));
        assert!(bob.to_str().unwrap().contains("bob"));
    }

    /// Given a valid token, username_from_token returns the correct username.
    #[test]
    fn username_from_token_finds_correct_user() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "r1", "alice", "tok-alice");
        let user = username_from_token_in(dir.path(), "r1", "tok-alice").unwrap();
        assert_eq!(user, "alice");
    }

    /// Multiple token files coexist — each resolves to the right username.
    #[test]
    fn username_from_token_disambiguates_multiple_users() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "r2", "alice", "tok-alice");
        write_token_file(dir.path(), "r2", "bob", "tok-bob");

        assert_eq!(
            username_from_token_in(dir.path(), "r2", "tok-alice").unwrap(),
            "alice"
        );
        assert_eq!(
            username_from_token_in(dir.path(), "r2", "tok-bob").unwrap(),
            "bob"
        );
    }

    /// Unknown token → clear error with join hint.
    #[test]
    fn username_from_token_unknown_errors_with_join_hint() {
        let dir = TempDir::new().unwrap();
        let err = username_from_token_in(dir.path(), "r3", "not-a-real-token").unwrap_err();
        assert!(
            err.to_string().contains("room join"),
            "expected 'room join' hint in: {err}"
        );
    }

    /// Two agents joining the same room write independent token files and neither
    /// overwrites the other — core regression for issue #40.
    #[test]
    fn two_agents_tokens_do_not_collide() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "r4", "alice", "tok-alice");
        write_token_file(dir.path(), "r4", "bob", "tok-bob");

        // Both files survive; each resolves to the right user via their token.
        assert_eq!(
            username_from_token_in(dir.path(), "r4", "tok-alice").unwrap(),
            "alice"
        );
        assert_eq!(
            username_from_token_in(dir.path(), "r4", "tok-bob").unwrap(),
            "bob"
        );
    }
}
