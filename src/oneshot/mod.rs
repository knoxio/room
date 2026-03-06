pub mod token;
pub mod transport;

pub use token::{cmd_join, token_file_path, username_from_token};
pub use transport::{join_session, send_message, send_message_with_token};

use std::path::{Path, PathBuf};

use crate::{history, message::Message};
use token::{read_cursor, write_cursor};
use transport::send_message_with_token as transport_send;

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
    let msg = transport_send(&socket_path, token, &wire)
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

/// Return the last `n` messages from history without updating the poll cursor.
///
/// DM entries are filtered so that `viewer` only sees messages where they are
/// the sender or the recipient. Pass `None` to skip DM filtering.
pub async fn pull_messages(
    chat_path: &Path,
    n: usize,
    viewer: Option<&str>,
) -> anyhow::Result<Vec<Message>> {
    let clamped = n.min(200);
    let all = history::tail(chat_path, clamped).await?;
    let visible: Vec<Message> = all
        .into_iter()
        .filter(|m| match m {
            Message::DirectMessage { user, to, .. } => viewer
                .map(|v| v == user.as_str() || v == to.as_str())
                .unwrap_or(true),
            _ => true,
        })
        .collect();
    Ok(visible)
}

/// One-shot pull subcommand: print the last N messages from history as NDJSON.
///
/// Reads from the chat file directly (no broker connection required).
/// Does **not** update the poll cursor.
pub async fn cmd_pull(room_id: &str, token: &str, n: usize) -> anyhow::Result<()> {
    let username = username_from_token(room_id, token)?;
    let meta_path = PathBuf::from(format!("/tmp/room-{room_id}.meta"));
    let chat_path = chat_path_from_meta(room_id, &meta_path);

    let messages = pull_messages(&chat_path, n, Some(&username)).await?;
    for msg in &messages {
        println!("{}", serde_json::to_string(msg)?);
    }
    Ok(())
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
