mod commands;
mod filter_events;
mod filter_tier;
pub(super) mod meta;
mod multi_room;

use std::path::Path;

use crate::{history, message::Message};

use super::token::{read_cursor, write_cursor};

// ── Public re-exports ─────────────────────────────────────────────────────────

pub use commands::{cmd_poll, cmd_poll_multi, cmd_pull, cmd_query, cmd_watch};
pub use multi_room::poll_messages_multi;

// ── Query engine types ─────────────────────────────────────────────────────────

/// Options for the `room query` subcommand (and `poll`/`watch` aliases).
#[derive(Debug, Clone)]
pub struct QueryOptions {
    /// Only return messages since the last poll cursor; advances the cursor
    /// after printing results.
    pub new_only: bool,
    /// Block until at least one new foreign message arrives (implies `new_only`).
    pub wait: bool,
    /// Poll interval in seconds when `wait` is true.
    pub interval_secs: u64,
    /// If `true`, only return messages that @mention the calling user
    /// (username resolved from the token).
    pub mentions_only: bool,
    /// Override the cursor with this legacy message UUID (used by the `poll`
    /// alias `--since` flag, which predates the `room:seq` format).
    pub since_uuid: Option<String>,
}

/// Return all messages from `chat_path` after the message with ID `since` (exclusive).
///
/// If `since` is `None`, the cursor file at `cursor_path` is checked for a previously
/// stored position. A `None` cursor means all messages are returned.
///
/// `viewer` is the username of the caller. When `Some`, `DirectMessage` entries are
/// filtered using [`Message::is_visible_to`], which grants access to the sender,
/// recipient, and the room host. Pass `None` to skip DM filtering (e.g. in tests
/// that don't involve DMs).
///
/// `host` is the room host username (typically the first user to join). When `Some`,
/// the host can see all DMs regardless of sender/recipient.
///
/// The cursor file is updated to the last returned message's ID after each successful call.
pub async fn poll_messages(
    chat_path: &Path,
    cursor_path: &Path,
    viewer: Option<&str>,
    host: Option<&str>,
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
        .filter(|m| viewer.map(|v| m.is_visible_to(v, host)).unwrap_or(true))
        .cloned()
        .collect();

    if let Some(last) = result.last() {
        write_cursor(cursor_path, last.id())?;
    }

    Ok(result)
}

/// Return the last `n` messages from history without updating the poll cursor.
///
/// DM entries are filtered using [`Message::is_visible_to`] so that `viewer` only
/// sees messages they are party to (sender, recipient, or host). Pass `None` to
/// skip DM filtering.
///
/// `host` is the room host username. When `Some`, the host can see all DMs.
pub async fn pull_messages(
    chat_path: &Path,
    n: usize,
    viewer: Option<&str>,
    host: Option<&str>,
) -> anyhow::Result<Vec<Message>> {
    let clamped = n.min(200);
    let all = history::tail(chat_path, clamped).await?;
    let visible: Vec<Message> = all
        .into_iter()
        .filter(|m| viewer.map(|v| m.is_visible_to(v, host)).unwrap_or(true))
        .collect();
    Ok(visible)
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

        let result = poll_messages(chat.path(), &cursor, None, None, None)
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

        poll_messages(chat.path(), &cursor, None, None, None)
            .await
            .unwrap();

        let second = poll_messages(chat.path(), &cursor, None, None, None)
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
        let result = poll_messages(chat.path(), &cursor, Some("bob"), None, None)
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
        let messages = poll_messages(chat.path(), &cursor, Some("bob"), None, None)
            .await
            .unwrap();

        let username = "bob";
        let foreign: Vec<&Message> = messages
            .iter()
            .filter(|m| match m {
                Message::Message { user, .. } | Message::System { user, .. } => user != username,
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

        let messages = poll_messages(chat.path(), &cursor, Some("bob"), None, None)
            .await
            .unwrap();

        let username = "bob";
        let foreign: Vec<&Message> = messages
            .iter()
            .filter(|m| match m {
                Message::Message { user, .. } | Message::System { user, .. } => user != username,
                Message::DirectMessage { to, .. } => to == username,
                _ => false,
            })
            .collect();

        assert!(
            foreign.is_empty(),
            "DMs sent by the watcher should not wake watch"
        );
    }

    /// System messages from other users wake the watch filter (#452).
    #[tokio::test]
    async fn watch_filter_wakes_on_foreign_system_message() {
        use room_protocol::make_system;
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        let sys = make_system("r", "plugin:taskboard", "task tb-001 approved");
        crate::history::append(chat.path(), &sys).await.unwrap();

        let messages = poll_messages(chat.path(), &cursor, Some("bob"), None, None)
            .await
            .unwrap();

        let username = "bob";
        let foreign: Vec<&Message> = messages
            .iter()
            .filter(|m| match m {
                Message::Message { user, .. } | Message::System { user, .. } => user != username,
                Message::DirectMessage { to, .. } => to == username,
                _ => false,
            })
            .collect();

        assert_eq!(
            foreign.len(),
            1,
            "system messages from other users should wake watch"
        );
        assert!(matches!(foreign[0], Message::System { .. }));
    }

    /// System messages from the watcher's own username do not wake watch.
    #[tokio::test]
    async fn watch_filter_ignores_own_system_message() {
        use room_protocol::make_system;
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        let sys = make_system("r", "bob", "bob subscribed (tier: full)");
        crate::history::append(chat.path(), &sys).await.unwrap();

        let messages = poll_messages(chat.path(), &cursor, Some("bob"), None, None)
            .await
            .unwrap();

        let username = "bob";
        let foreign: Vec<&Message> = messages
            .iter()
            .filter(|m| match m {
                Message::Message { user, .. } | Message::System { user, .. } => user != username,
                Message::DirectMessage { to, .. } => to == username,
                _ => false,
            })
            .collect();

        assert!(
            foreign.is_empty(),
            "system messages from self should not wake watch"
        );
    }

    /// Watch filter handles a mix of messages, system events, and DMs correctly.
    #[tokio::test]
    async fn watch_filter_mixed_message_types() {
        use crate::message::make_dm;
        use room_protocol::make_system;
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        // Foreign regular message
        let msg = make_message("r", "alice", "hello");
        // Foreign system message (plugin broadcast)
        let sys = make_system("r", "plugin:taskboard", "task tb-001 claimed by alice");
        // Own system message (should be filtered out)
        let own_sys = make_system("r", "bob", "bob subscribed (tier: full)");
        // DM addressed to watcher
        let dm = make_dm("r", "alice", "bob", "private note");
        // Own regular message (should be filtered out)
        let own_msg = make_message("r", "bob", "my own message");

        for m in [&msg, &sys, &own_sys, &dm, &own_msg] {
            crate::history::append(chat.path(), m).await.unwrap();
        }

        let messages = poll_messages(chat.path(), &cursor, Some("bob"), None, None)
            .await
            .unwrap();

        let username = "bob";
        let foreign: Vec<&Message> = messages
            .iter()
            .filter(|m| match m {
                Message::Message { user, .. } | Message::System { user, .. } => user != username,
                Message::DirectMessage { to, .. } => to == username,
                _ => false,
            })
            .collect();

        assert_eq!(
            foreign.len(),
            3,
            "should see: foreign message + foreign system + DM to self"
        );
        assert!(
            foreign.iter().any(|m| matches!(m, Message::System { .. })),
            "system message must appear in watch results"
        );
        assert!(
            foreign.iter().any(|m| matches!(m, Message::Message { .. })),
            "regular foreign message must appear"
        );
        assert!(
            foreign
                .iter()
                .any(|m| matches!(m, Message::DirectMessage { .. })),
            "DM to self must appear"
        );
    }

    /// Host sees all DMs in poll regardless of sender/recipient.
    #[tokio::test]
    async fn poll_messages_host_sees_all_dms() {
        use crate::message::make_dm;
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        let dm_alice_bob = make_dm("r", "alice", "bob", "private");
        let dm_carol_dave = make_dm("r", "carol", "dave", "also private");
        crate::history::append(chat.path(), &dm_alice_bob)
            .await
            .unwrap();
        crate::history::append(chat.path(), &dm_carol_dave)
            .await
            .unwrap();

        // host "eve" can see both DMs
        let result = poll_messages(chat.path(), &cursor, Some("eve"), Some("eve"), None)
            .await
            .unwrap();
        assert_eq!(result.len(), 2, "host should see all DMs");
    }

    /// Non-host third party cannot see DMs they are not party to.
    #[tokio::test]
    async fn poll_messages_non_host_cannot_see_unrelated_dms() {
        use crate::message::make_dm;
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let cursor = cursor_dir.path().join("cursor");

        let dm = make_dm("r", "alice", "bob", "private");
        crate::history::append(chat.path(), &dm).await.unwrap();

        // carol is not a party and is not host
        let result = poll_messages(chat.path(), &cursor, Some("carol"), None, None)
            .await
            .unwrap();
        assert!(result.is_empty(), "non-host third party should not see DM");
    }

    /// Host reads from pull_messages as well.
    #[tokio::test]
    async fn pull_messages_host_sees_all_dms() {
        use crate::message::make_dm;
        let chat = NamedTempFile::new().unwrap();

        let dm = make_dm("r", "alice", "bob", "secret");
        crate::history::append(chat.path(), &dm).await.unwrap();

        let result = pull_messages(chat.path(), 10, Some("eve"), Some("eve"))
            .await
            .unwrap();
        assert_eq!(result.len(), 1, "host should see the DM via pull");
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

        let pulled = pull_messages(chat.path(), 3, None, None).await.unwrap();
        assert_eq!(pulled.len(), 3);

        // cursor untouched — poll still returns all 5
        let polled = poll_messages(chat.path(), &cursor, None, None, None)
            .await
            .unwrap();
        assert_eq!(polled.len(), 5);
    }
}
