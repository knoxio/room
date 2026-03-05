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

/// One-shot send subcommand: connect, send, print echo JSON to stdout, exit.
pub async fn cmd_send(room_id: &str, username: &str, content: &str) -> anyhow::Result<()> {
    let socket_path = PathBuf::from(format!("/tmp/room-{room_id}.sock"));
    let msg = send_message(&socket_path, username, content).await?;
    println!("{}", serde_json::to_string(&msg)?);
    Ok(())
}

/// Return all messages from `chat_path` after the message with ID `since` (exclusive).
///
/// If `since` is `None`, the cursor file at `cursor_path` is checked for a previously
/// stored position. A `None` cursor means all messages are returned.
///
/// The cursor file is updated to the last returned message's ID after each successful call.
pub async fn poll_messages(
    chat_path: &Path,
    cursor_path: &Path,
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

    let result: Vec<Message> = messages[start..].to_vec();

    if let Some(last) = result.last() {
        write_cursor(cursor_path, last.id())?;
    }

    Ok(result)
}

/// One-shot poll subcommand: read messages since cursor, print as NDJSON, update cursor.
pub async fn cmd_poll(
    room_id: &str,
    username: &str,
    since: Option<String>,
) -> anyhow::Result<()> {
    let meta_path = PathBuf::from(format!("/tmp/room-{room_id}.meta"));
    let chat_path = chat_path_from_meta(room_id, &meta_path);
    let cursor_path = PathBuf::from(format!("/tmp/room-{room_id}-{username}.cursor"));

    let messages = poll_messages(&chat_path, &cursor_path, since.as_deref()).await?;
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
