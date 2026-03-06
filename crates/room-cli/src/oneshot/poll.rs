use std::path::{Path, PathBuf};

use crate::{history, message::Message};

use super::token::{read_cursor, username_from_token, write_cursor};

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
            .filter(|m| match m {
                Message::Message { user, .. } => user != &username,
                Message::DirectMessage { to, .. } => to == &username,
                _ => false,
            })
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

pub(super) fn chat_path_from_meta(room_id: &str, meta_path: &Path) -> PathBuf {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::make_message;
    use tempfile::{NamedTempFile, TempDir};

    /// poll_messages with no cursor and no since returns all messages.
    #[tokio::test]
    async fn poll_messages_no_cursor_returns_all() {
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        let msg = make_message("r", "alice", "hello");
        crate::history::append(chat.path(), &msg).await.unwrap();

        let result = poll_messages(chat.path(), &cursor, None, None)
            .await
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id(), msg.id());
    }

    /// poll_messages advances the cursor so a second call returns nothing.
    #[tokio::test]
    async fn poll_messages_advances_cursor() {
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        let msg = make_message("r", "alice", "hello");
        crate::history::append(chat.path(), &msg).await.unwrap();

        poll_messages(chat.path(), &cursor, None, None)
            .await
            .unwrap();

        let second = poll_messages(chat.path(), &cursor, None, None)
            .await
            .unwrap();
        assert!(
            second.is_empty(),
            "cursor should have advanced past the first message"
        );
    }

    /// DM visibility: viewer only sees DMs they sent or received.
    #[tokio::test]
    async fn poll_messages_filters_dms_by_viewer() {
        use crate::message::make_dm;
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        let dm_alice_bob = make_dm("r", "alice", "bob", "secret");
        let dm_alice_carol = make_dm("r", "alice", "carol", "other secret");
        crate::history::append(chat.path(), &dm_alice_bob)
            .await
            .unwrap();
        crate::history::append(chat.path(), &dm_alice_carol)
            .await
            .unwrap();

        // bob sees only his DM
        let result = poll_messages(chat.path(), &cursor, Some("bob"), None)
            .await
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id(), dm_alice_bob.id());
    }

    /// DMs addressed to the watcher are included in the foreign message filter
    /// used by cmd_watch, not silently consumed.
    #[tokio::test]
    async fn poll_messages_dm_to_viewer_is_not_consumed_silently() {
        use crate::message::make_dm;
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        // alice sends a DM to bob, and a broadcast message
        let dm = make_dm("r", "alice", "bob", "secret for bob");
        let msg = make_message("r", "alice", "public hello");
        crate::history::append(chat.path(), &dm).await.unwrap();
        crate::history::append(chat.path(), &msg).await.unwrap();

        // Simulate what cmd_watch does: poll, then filter for foreign messages + DMs
        let messages = poll_messages(chat.path(), &cursor, Some("bob"), None)
            .await
            .unwrap();

        let username = "bob";
        let foreign: Vec<&Message> = messages
            .iter()
            .filter(|m| match m {
                Message::Message { user, .. } => user != username,
                Message::DirectMessage { to, .. } => to == username,
                _ => false,
            })
            .collect();

        // Both the DM (addressed to bob) and the broadcast (from alice) should appear
        assert_eq!(foreign.len(), 2, "watch should see DMs + foreign messages");
        assert!(
            foreign
                .iter()
                .any(|m| matches!(m, Message::DirectMessage { .. })),
            "DM must not be silently consumed"
        );
    }

    /// DMs sent BY the watcher are excluded from the foreign filter (no self-echo).
    #[tokio::test]
    async fn poll_messages_dm_from_viewer_excluded_from_watch() {
        use crate::message::make_dm;
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        // bob sends a DM to alice
        let dm = make_dm("r", "bob", "alice", "from bob");
        crate::history::append(chat.path(), &dm).await.unwrap();

        let messages = poll_messages(chat.path(), &cursor, Some("bob"), None)
            .await
            .unwrap();

        let username = "bob";
        let foreign: Vec<&Message> = messages
            .iter()
            .filter(|m| match m {
                Message::Message { user, .. } => user != username,
                Message::DirectMessage { to, .. } => to == username,
                _ => false,
            })
            .collect();

        assert!(
            foreign.is_empty(),
            "DMs sent by the watcher should not wake watch"
        );
    }

    /// pull_messages returns the last n entries without moving the cursor.
    #[tokio::test]
    async fn pull_messages_returns_tail_without_cursor_change() {
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        for i in 0..5u32 {
            crate::history::append(chat.path(), &make_message("r", "u", format!("msg {i}")))
                .await
                .unwrap();
        }

        let pulled = pull_messages(chat.path(), 3, None).await.unwrap();
        assert_eq!(pulled.len(), 3);

        // cursor untouched — poll still returns all 5
        let polled = poll_messages(chat.path(), &cursor, None, None)
            .await
            .unwrap();
        assert_eq!(polled.len(), 5);
    }
}
