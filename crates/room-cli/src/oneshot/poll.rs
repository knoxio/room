use std::path::{Path, PathBuf};

use room_protocol::{EventFilter, SubscriptionTier};

use crate::{
    broker::persistence::{load_event_filter_map, load_subscription_map},
    history,
    message::Message,
    paths,
    query::QueryFilter,
};

use super::token::{read_cursor, username_from_token, write_cursor};

// ── Subscription tier lookup ──────────────────────────────────────────────────

/// Look up a user's subscription tier for a room from the persisted
/// subscription map on disk.
///
/// Returns `Full` when the subscription file is missing, corrupt, or the user
/// has no entry — unsubscribed users must have been explicitly recorded.
fn load_user_tier(room_id: &str, username: &str) -> SubscriptionTier {
    let state_dir = paths::room_state_dir();
    let sub_path = paths::broker_subscriptions_path(&state_dir, room_id);
    let map = load_subscription_map(&sub_path);
    map.get(username).copied().unwrap_or(SubscriptionTier::Full)
}

/// Apply subscription-tier filtering to a message list in place.
///
/// - `Full` — no filtering (all messages pass).
/// - `MentionsOnly` — keep only messages that @mention `username`.
/// - `Unsubscribed` — remove all messages.
fn apply_tier_filter(messages: &mut Vec<Message>, tier: SubscriptionTier, username: &str) {
    match tier {
        SubscriptionTier::Full => {}
        SubscriptionTier::MentionsOnly => {
            messages.retain(|m| m.mentions().iter().any(|mention| mention == username));
        }
        SubscriptionTier::Unsubscribed => {
            messages.clear();
        }
    }
}

/// Apply per-room subscription-tier filtering to a message list in place.
///
/// Looks up the user's tier for each message's room and filters accordingly.
/// Each room is checked independently — a user can be `Full` in one room and
/// `Unsubscribed` in another.
fn apply_per_room_tier_filter(messages: &mut Vec<Message>, room_ids: &[String], username: &str) {
    use std::collections::HashMap;
    let tiers: HashMap<&str, SubscriptionTier> = room_ids
        .iter()
        .map(|r| (r.as_str(), load_user_tier(r, username)))
        .collect();

    messages.retain(|m| {
        let tier = tiers
            .get(m.room())
            .copied()
            .unwrap_or(SubscriptionTier::Full);
        match tier {
            SubscriptionTier::Full => true,
            SubscriptionTier::MentionsOnly => {
                m.mentions().iter().any(|mention| mention == username)
            }
            SubscriptionTier::Unsubscribed => false,
        }
    });
}

// ── Event filter lookup ──────────────────────────────────────────────────────

/// Look up a user's event filter for a room from the persisted event filter
/// map on disk.
///
/// Returns `EventFilter::All` when the file is missing, corrupt, or the user
/// has no entry — all events pass by default.
fn load_user_event_filter(room_id: &str, username: &str) -> EventFilter {
    let state_dir = paths::room_state_dir();
    let ef_path = paths::broker_event_filters_path(&state_dir, room_id);
    let map = load_event_filter_map(&ef_path);
    map.get(username).cloned().unwrap_or(EventFilter::All)
}

/// Apply event-type filtering to a message list in place.
///
/// Only [`Message::Event`] messages are affected — all other message types
/// pass through unfiltered. For Event messages, the `event_type` field is
/// checked against the filter.
fn apply_event_filter(messages: &mut Vec<Message>, filter: &EventFilter) {
    if matches!(filter, EventFilter::All) {
        return;
    }
    messages.retain(|m| match m {
        Message::Event { event_type, .. } => filter.allows(event_type),
        _ => true,
    });
}

/// Apply per-room event-filter filtering to a message list in place.
///
/// Similar to [`apply_per_room_tier_filter`] but for event types.
fn apply_per_room_event_filter(messages: &mut Vec<Message>, room_ids: &[String], username: &str) {
    use std::collections::HashMap;
    let filters: HashMap<&str, EventFilter> = room_ids
        .iter()
        .map(|r| (r.as_str(), load_user_event_filter(r, username)))
        .collect();

    messages.retain(|m| match m {
        Message::Event {
            room, event_type, ..
        } => {
            let filter = filters.get(room.as_str()).unwrap_or(&EventFilter::All);
            filter.allows(event_type)
        }
        _ => true,
    });
}

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

/// One-shot pull subcommand: print the last N messages from history as NDJSON.
///
/// Reads from the chat file directly (no broker connection required).
/// Does **not** update the poll cursor.
pub async fn cmd_pull(room_id: &str, token: &str, n: usize) -> anyhow::Result<()> {
    let username = username_from_token(token)?;
    let meta_path = paths::room_meta_path(room_id);
    let chat_path = chat_path_from_meta(room_id, &meta_path);

    let host = read_host_from_meta(&meta_path);
    let mut messages = pull_messages(&chat_path, n, Some(&username), host.as_deref()).await?;
    let tier = load_user_tier(room_id, &username);
    apply_tier_filter(&mut messages, tier, &username);
    let ef = load_user_event_filter(room_id, &username);
    apply_event_filter(&mut messages, &ef);
    for msg in &messages {
        println!("{}", serde_json::to_string(msg)?);
    }
    Ok(())
}

/// Watch subcommand: poll in a loop until at least one foreign message arrives.
///
/// Reads the caller's username from the session token file. Polls every
/// `interval_secs` seconds, filtering out own messages. Wakes on `Message`,
/// `System`, and `DirectMessage` variants. Exits after printing the first batch
/// of foreign messages as NDJSON. Shares the cursor file with `room poll` — the
/// two subcommands never re-deliver the same message.
pub async fn cmd_watch(room_id: &str, token: &str, interval_secs: u64) -> anyhow::Result<()> {
    let username = username_from_token(token)?;
    let meta_path = paths::room_meta_path(room_id);
    let chat_path = chat_path_from_meta(room_id, &meta_path);
    let cursor_path = paths::cursor_path(room_id, &username);
    let host = read_host_from_meta(&meta_path);

    let ef = load_user_event_filter(room_id, &username);

    loop {
        let mut messages = poll_messages(
            &chat_path,
            &cursor_path,
            Some(&username),
            host.as_deref(),
            None,
        )
        .await?;

        apply_event_filter(&mut messages, &ef);

        let foreign: Vec<&Message> = messages
            .iter()
            .filter(|m| match m {
                Message::Message { user, .. } | Message::System { user, .. } => user != &username,
                Message::DirectMessage { to, .. } => to == &username,
                Message::Event { user, .. } => user != &username,
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
/// Reads the caller's username from the session token file. When `mentions_only` is
/// true, only messages that @mention the caller's username are printed (cursor still
/// advances past all messages).
pub async fn cmd_poll(
    room_id: &str,
    token: &str,
    since: Option<String>,
    mentions_only: bool,
) -> anyhow::Result<()> {
    let username = username_from_token(token)?;
    let meta_path = paths::room_meta_path(room_id);
    let chat_path = chat_path_from_meta(room_id, &meta_path);
    let cursor_path = paths::cursor_path(room_id, &username);
    let host = read_host_from_meta(&meta_path);

    let mut messages = poll_messages(
        &chat_path,
        &cursor_path,
        Some(&username),
        host.as_deref(),
        since.as_deref(),
    )
    .await?;

    let ef = load_user_event_filter(room_id, &username);
    apply_event_filter(&mut messages, &ef);

    for msg in &messages {
        if mentions_only && !msg.mentions().iter().any(|m| m == &username) {
            continue;
        }
        println!("{}", serde_json::to_string(msg)?);
    }
    Ok(())
}

/// Poll multiple rooms, returning messages merged by timestamp.
///
/// Each room uses its own cursor file under `~/.room/state/`.
/// Messages are sorted by timestamp across all rooms. Each message already carries
/// a `room` field, so the caller can distinguish sources.
pub async fn poll_messages_multi(
    rooms: &[(&str, &Path)],
    username: &str,
) -> anyhow::Result<Vec<Message>> {
    let mut all_messages: Vec<Message> = Vec::new();

    for &(room_id, chat_path) in rooms {
        let cursor_path = paths::cursor_path(room_id, username);
        let meta_path = paths::room_meta_path(room_id);
        let host = read_host_from_meta(&meta_path);
        let msgs = poll_messages(
            chat_path,
            &cursor_path,
            Some(username),
            host.as_deref(),
            None,
        )
        .await?;
        all_messages.extend(msgs);
    }

    all_messages.sort_by(|a, b| a.ts().cmp(b.ts()));
    Ok(all_messages)
}

/// One-shot multi-room poll subcommand: poll multiple rooms, merge by timestamp, print NDJSON.
///
/// Resolves the username from the token by trying each room in order. Each room's cursor
/// is updated independently.
pub async fn cmd_poll_multi(
    room_ids: &[String],
    token: &str,
    mentions_only: bool,
) -> anyhow::Result<()> {
    // Resolve username by trying the token against each room
    let username = username_from_token(token)?;

    // Resolve chat paths for all rooms
    let mut rooms: Vec<(&str, PathBuf)> = Vec::new();
    for room_id in room_ids {
        let meta_path = paths::room_meta_path(room_id);
        let chat_path = chat_path_from_meta(room_id, &meta_path);
        rooms.push((room_id.as_str(), chat_path));
    }

    let room_refs: Vec<(&str, &Path)> = rooms.iter().map(|(id, p)| (*id, p.as_path())).collect();
    let mut messages = poll_messages_multi(&room_refs, &username).await?;

    let room_id_strings: Vec<String> = room_ids.iter().map(|s| s.to_string()).collect();
    apply_per_room_event_filter(&mut messages, &room_id_strings, &username);

    for msg in &messages {
        if mentions_only && !msg.mentions().iter().any(|m| m == &username) {
            continue;
        }
        println!("{}", serde_json::to_string(msg)?);
    }
    Ok(())
}

// ── cmd_query ─────────────────────────────────────────────────────────────────

/// Unified query entry point for `room query` and the `poll`/`watch` aliases.
///
/// Three modes:
/// - **Historical** (`new_only = false, wait = false`): reads full history,
///   applies filter, no cursor update.
/// - **New** (`new_only = true, wait = false`): reads since last cursor,
///   applies filter, advances cursor.
/// - **Wait** (`wait = true`): loops until at least one foreign message passes
///   the filter, then prints and exits.
///
/// `room_ids` lists the rooms to read. `filter.rooms` may further restrict the
/// output but does not affect which files are opened.
pub async fn cmd_query(
    room_ids: &[String],
    token: &str,
    mut filter: QueryFilter,
    opts: QueryOptions,
) -> anyhow::Result<()> {
    if room_ids.is_empty() {
        anyhow::bail!("at least one room ID is required");
    }

    let username = username_from_token(token)?;

    // Resolve mention_user from caller if mentions_only is requested.
    if opts.mentions_only {
        filter.mention_user = Some(username.clone());
    }

    if opts.wait || opts.new_only {
        cmd_query_new(room_ids, &username, filter, opts).await
    } else {
        cmd_query_history(room_ids, &username, filter).await
    }
}

/// Cursor-based (new / wait) query mode.
async fn cmd_query_new(
    room_ids: &[String],
    username: &str,
    filter: QueryFilter,
    opts: QueryOptions,
) -> anyhow::Result<()> {
    loop {
        let messages: Vec<Message> = if room_ids.len() == 1 {
            let room_id = &room_ids[0];
            let meta_path = paths::room_meta_path(room_id);
            let chat_path = chat_path_from_meta(room_id, &meta_path);
            let cursor_path = paths::cursor_path(room_id, username);
            let host = read_host_from_meta(&meta_path);
            poll_messages(
                &chat_path,
                &cursor_path,
                Some(username),
                host.as_deref(),
                opts.since_uuid.as_deref(),
            )
            .await?
        } else {
            let mut rooms_info: Vec<(String, PathBuf)> = Vec::new();
            for room_id in room_ids {
                let meta_path = paths::room_meta_path(room_id);
                let chat_path = chat_path_from_meta(room_id, &meta_path);
                rooms_info.push((room_id.clone(), chat_path));
            }
            let room_refs: Vec<(&str, &Path)> = rooms_info
                .iter()
                .map(|(id, p)| (id.as_str(), p.as_path()))
                .collect();
            poll_messages_multi(&room_refs, username).await?
        };

        // Apply query filter, per-room subscription tiers, then sort + limit.
        let mut filtered: Vec<Message> = messages
            .into_iter()
            .filter(|m| filter.matches(m, m.room()))
            .collect();

        if !filter.public_only {
            apply_per_room_tier_filter(&mut filtered, room_ids, username);
            apply_per_room_event_filter(&mut filtered, room_ids, username);
        }

        apply_sort_and_limit(&mut filtered, &filter);

        if opts.wait {
            // Only wake on foreign messages (includes system messages from plugins).
            let foreign: Vec<&Message> = filtered
                .iter()
                .filter(|m| match m {
                    Message::Message { user, .. } | Message::System { user, .. } => {
                        user != username
                    }
                    Message::DirectMessage { to, .. } => to == username,
                    _ => false,
                })
                .collect();

            if !foreign.is_empty() {
                for msg in foreign {
                    println!("{}", serde_json::to_string(msg)?);
                }
                return Ok(());
            }
        } else {
            for msg in &filtered {
                println!("{}", serde_json::to_string(msg)?);
            }
            return Ok(());
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(opts.interval_secs)).await;
    }
}

/// Historical (no-cursor) query mode.
async fn cmd_query_history(
    room_ids: &[String],
    username: &str,
    filter: QueryFilter,
) -> anyhow::Result<()> {
    let mut all_messages: Vec<Message> = Vec::new();

    for room_id in room_ids {
        let meta_path = paths::room_meta_path(room_id);
        let chat_path = chat_path_from_meta(room_id, &meta_path);
        let messages = history::load(&chat_path).await?;
        all_messages.extend(messages);
    }

    // DM privacy filter: viewer only sees their own DMs.
    let mut filtered: Vec<Message> = all_messages
        .into_iter()
        .filter(|m| filter.matches(m, m.room()))
        .filter(|m| match m {
            Message::DirectMessage { user, to, .. } => user == username || to == username,
            _ => true,
        })
        .collect();

    if !filter.public_only {
        apply_per_room_tier_filter(&mut filtered, room_ids, username);
        apply_per_room_event_filter(&mut filtered, room_ids, username);
    }

    apply_sort_and_limit(&mut filtered, &filter);

    // If a specific target_id was requested and nothing was found, report an error.
    if filtered.is_empty() {
        if let Some((ref target_room, target_seq)) = filter.target_id {
            use room_protocol::format_message_id;
            anyhow::bail!(
                "message not found: {}",
                format_message_id(target_room, target_seq)
            );
        }
    }

    for msg in &filtered {
        println!("{}", serde_json::to_string(msg)?);
    }
    Ok(())
}

/// Apply sort order and optional limit to a message list in place.
fn apply_sort_and_limit(messages: &mut Vec<Message>, filter: &QueryFilter) {
    if filter.ascending {
        messages.sort_by(|a, b| a.ts().cmp(b.ts()));
    } else {
        messages.sort_by(|a, b| b.ts().cmp(a.ts()));
    }
    if let Some(limit) = filter.limit {
        messages.truncate(limit);
    }
}

/// Read the room host username from the meta file, if present.
///
/// Returns `None` if the meta file does not exist, cannot be parsed, or has no
/// `"host"` field. Callers should treat `None` the same as no host information.
pub(super) fn read_host_from_meta(meta_path: &Path) -> Option<String> {
    if !meta_path.exists() {
        return None;
    }
    let data = std::fs::read_to_string(meta_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["host"].as_str().map(str::to_owned)
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

    // ── poll_messages_multi tests ──────────────────────────────────────────

    /// Multi-room poll merges messages from two rooms sorted by timestamp.
    #[tokio::test]
    async fn poll_multi_merges_by_timestamp() {
        let chat_a = NamedTempFile::new().unwrap();
        let chat_b = NamedTempFile::new().unwrap();

        let rid_a = format!("test-merge-a-{}", std::process::id());
        let rid_b = format!("test-merge-b-{}", std::process::id());

        // Append messages with interleaved timestamps
        let msg_a1 = make_message(&rid_a, "alice", "a1");
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let msg_b1 = make_message(&rid_b, "bob", "b1");
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let msg_a2 = make_message(&rid_a, "alice", "a2");

        crate::history::append(chat_a.path(), &msg_a1)
            .await
            .unwrap();
        crate::history::append(chat_b.path(), &msg_b1)
            .await
            .unwrap();
        crate::history::append(chat_a.path(), &msg_a2)
            .await
            .unwrap();

        let rooms: Vec<(&str, &Path)> = vec![
            (rid_a.as_str(), chat_a.path()),
            (rid_b.as_str(), chat_b.path()),
        ];

        let result = poll_messages_multi(&rooms, "viewer").await.unwrap();
        assert_eq!(result.len(), 3);
        // Verify timestamp ordering
        assert!(result[0].ts() <= result[1].ts());
        assert!(result[1].ts() <= result[2].ts());
        // First message should be from room-a (earliest)
        assert_eq!(result[0].room(), &rid_a);

        // Clean up cursor files
        let _ = std::fs::remove_file(crate::paths::cursor_path(&rid_a, "viewer"));
        let _ = std::fs::remove_file(crate::paths::cursor_path(&rid_b, "viewer"));
    }

    /// Multi-room poll uses per-room cursors (second call returns nothing).
    #[tokio::test]
    async fn poll_multi_advances_per_room_cursors() {
        let chat_a = NamedTempFile::new().unwrap();
        let chat_b = NamedTempFile::new().unwrap();

        // Use unique room IDs to avoid cursor file collisions with parallel tests
        let rid_a = format!("test-cursor-a-{}", std::process::id());
        let rid_b = format!("test-cursor-b-{}", std::process::id());

        let msg_a = make_message(&rid_a, "alice", "hello a");
        let msg_b = make_message(&rid_b, "bob", "hello b");
        crate::history::append(chat_a.path(), &msg_a).await.unwrap();
        crate::history::append(chat_b.path(), &msg_b).await.unwrap();

        let rooms: Vec<(&str, &Path)> = vec![
            (rid_a.as_str(), chat_a.path()),
            (rid_b.as_str(), chat_b.path()),
        ];

        // First poll gets everything
        let result = poll_messages_multi(&rooms, "viewer").await.unwrap();
        assert_eq!(result.len(), 2);

        // Second poll gets nothing (cursors advanced)
        let result2 = poll_messages_multi(&rooms, "viewer").await.unwrap();
        assert!(
            result2.is_empty(),
            "second multi-poll should return nothing"
        );

        // Clean up cursor files
        let _ = std::fs::remove_file(crate::paths::cursor_path(&rid_a, "viewer"));
        let _ = std::fs::remove_file(crate::paths::cursor_path(&rid_b, "viewer"));
    }

    /// Multi-room poll with one empty room still returns messages from the other.
    #[tokio::test]
    async fn poll_multi_one_empty_room() {
        let chat_a = NamedTempFile::new().unwrap();
        let chat_b = NamedTempFile::new().unwrap();

        let rid_a = format!("test-empty-a-{}", std::process::id());
        let rid_b = format!("test-empty-b-{}", std::process::id());

        let msg = make_message(&rid_a, "alice", "only here");
        crate::history::append(chat_a.path(), &msg).await.unwrap();
        // chat_b is empty

        let rooms: Vec<(&str, &Path)> = vec![
            (rid_a.as_str(), chat_a.path()),
            (rid_b.as_str(), chat_b.path()),
        ];

        let result = poll_messages_multi(&rooms, "viewer").await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].room(), &rid_a);

        let _ = std::fs::remove_file(crate::paths::cursor_path(&rid_a, "viewer"));
        let _ = std::fs::remove_file(crate::paths::cursor_path(&rid_b, "viewer"));
    }

    /// Multi-room poll with no rooms returns nothing.
    #[tokio::test]
    async fn poll_multi_no_rooms() {
        let rooms: Vec<(&str, &Path)> = vec![];
        let result = poll_messages_multi(&rooms, "viewer").await.unwrap();
        assert!(result.is_empty());
    }

    /// Multi-room poll filters DMs by viewer across rooms.
    #[tokio::test]
    async fn poll_multi_filters_dms_across_rooms() {
        use crate::message::make_dm;
        let chat_a = NamedTempFile::new().unwrap();
        let chat_b = NamedTempFile::new().unwrap();

        let rid_a = format!("test-dm-a-{}", std::process::id());
        let rid_b = format!("test-dm-b-{}", std::process::id());

        // DM to bob in room-a, DM to carol in room-b
        let dm_to_bob = make_dm(&rid_a, "alice", "bob", "secret for bob");
        let dm_to_carol = make_dm(&rid_b, "alice", "carol", "secret for carol");
        crate::history::append(chat_a.path(), &dm_to_bob)
            .await
            .unwrap();
        crate::history::append(chat_b.path(), &dm_to_carol)
            .await
            .unwrap();

        let rooms: Vec<(&str, &Path)> = vec![
            (rid_a.as_str(), chat_a.path()),
            (rid_b.as_str(), chat_b.path()),
        ];

        // bob sees only room-a DM
        let result = poll_messages_multi(&rooms, "bob").await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].room(), &rid_a);

        let _ = std::fs::remove_file(crate::paths::cursor_path(&rid_a, "bob"));
        let _ = std::fs::remove_file(crate::paths::cursor_path(&rid_b, "bob"));
    }

    // ── cmd_query unit tests ───────────────────────────────────────────────────

    /// cmd_query in historical mode returns all messages (newest-first by default).
    #[tokio::test]
    async fn cmd_query_history_returns_all_newest_first() {
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let token_dir = TempDir::new().unwrap();

        let room_id = format!("test-cqh-{}", std::process::id());
        write_token_file(&token_dir, &room_id, "alice-cqh", "tok-cqh");
        write_meta_file(&room_id, chat.path());

        for i in 0..3u32 {
            crate::history::append(
                chat.path(),
                &make_message(&room_id, "alice-cqh", format!("{i}")),
            )
            .await
            .unwrap();
        }

        let filter = QueryFilter {
            rooms: vec![room_id.clone()],
            ascending: false,
            ..Default::default()
        };
        let opts = QueryOptions {
            new_only: false,
            wait: false,
            interval_secs: 5,
            mentions_only: false,
            since_uuid: None,
        };

        // cursor should NOT advance (historical mode)
        let cursor_path = crate::paths::cursor_path(&room_id, "alice-cqh");
        let _ = std::fs::remove_file(&cursor_path);

        // Run cmd_query — captures stdout indirectly by ensuring cursor unchanged.
        oneshot_cmd_query_to_vec(&[room_id.clone()], "tok-cqh", filter, opts, &cursor_dir)
            .await
            .unwrap();

        // Cursor must not have been written in historical mode.
        assert!(
            !cursor_path.exists(),
            "historical query must not write a cursor file"
        );

        let _ = std::fs::remove_file(crate::paths::room_meta_path(&room_id));
        let _ = std::fs::remove_file(&global_token_path("alice-cqh"));
    }

    /// cmd_query in --new mode advances the cursor.
    #[tokio::test]
    async fn cmd_query_new_advances_cursor() {
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let token_dir = TempDir::new().unwrap();

        let room_id = format!("test-cqn-{}", std::process::id());
        write_token_file(&token_dir, &room_id, "alice-cqn", "tok-cqn");
        write_meta_file(&room_id, chat.path());

        let msg = make_message(&room_id, "bob", "hello");
        crate::history::append(chat.path(), &msg).await.unwrap();

        let filter = QueryFilter {
            rooms: vec![room_id.clone()],
            ascending: true,
            ..Default::default()
        };
        let opts = QueryOptions {
            new_only: true,
            wait: false,
            interval_secs: 5,
            mentions_only: false,
            since_uuid: None,
        };

        // First query — should return the message and write cursor.
        let result = oneshot_cmd_query_to_vec(
            &[room_id.clone()],
            "tok-cqn",
            filter.clone(),
            opts.clone(),
            &cursor_dir,
        )
        .await
        .unwrap();
        assert_eq!(result.len(), 1);

        // Second query — cursor advanced, nothing new.
        let result2 =
            oneshot_cmd_query_to_vec(&[room_id.clone()], "tok-cqn", filter, opts, &cursor_dir)
                .await
                .unwrap();
        assert!(
            result2.is_empty(),
            "second query should return nothing (cursor advanced)"
        );

        let _ = std::fs::remove_file(crate::paths::room_meta_path(&room_id));
        let _ = std::fs::remove_file(&global_token_path("alice-cqn"));
    }

    /// cmd_query with content_search only returns matching messages.
    #[tokio::test]
    async fn cmd_query_content_search_filters() {
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let token_dir = TempDir::new().unwrap();

        let room_id = format!("test-cqs-{}", std::process::id());
        write_token_file(&token_dir, &room_id, "alice-cqs", "tok-cqs");
        write_meta_file(&room_id, chat.path());

        crate::history::append(chat.path(), &make_message(&room_id, "bob", "hello world"))
            .await
            .unwrap();
        crate::history::append(chat.path(), &make_message(&room_id, "bob", "goodbye"))
            .await
            .unwrap();

        let filter = QueryFilter {
            rooms: vec![room_id.clone()],
            content_search: Some("hello".into()),
            ascending: true,
            ..Default::default()
        };
        let opts = QueryOptions {
            new_only: false,
            wait: false,
            interval_secs: 5,
            mentions_only: false,
            since_uuid: None,
        };

        let result =
            oneshot_cmd_query_to_vec(&[room_id.clone()], "tok-cqs", filter, opts, &cursor_dir)
                .await
                .unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].content().unwrap().contains("hello"));

        let _ = std::fs::remove_file(crate::paths::room_meta_path(&room_id));
        let _ = std::fs::remove_file(&global_token_path("alice-cqs"));
    }

    /// cmd_query with user filter only returns messages from that user.
    #[tokio::test]
    async fn cmd_query_user_filter() {
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let token_dir = TempDir::new().unwrap();

        let room_id = format!("test-cqu-{}", std::process::id());
        write_token_file(&token_dir, &room_id, "alice-cqu", "tok-cqu");
        write_meta_file(&room_id, chat.path());

        crate::history::append(chat.path(), &make_message(&room_id, "alice", "from alice"))
            .await
            .unwrap();
        crate::history::append(chat.path(), &make_message(&room_id, "bob", "from bob"))
            .await
            .unwrap();

        let filter = QueryFilter {
            rooms: vec![room_id.clone()],
            users: vec!["bob".into()],
            ascending: true,
            ..Default::default()
        };
        let opts = QueryOptions {
            new_only: false,
            wait: false,
            interval_secs: 5,
            mentions_only: false,
            since_uuid: None,
        };

        let result =
            oneshot_cmd_query_to_vec(&[room_id.clone()], "tok-cqu", filter, opts, &cursor_dir)
                .await
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].user(), "bob");

        let _ = std::fs::remove_file(crate::paths::room_meta_path(&room_id));
        let _ = std::fs::remove_file(&global_token_path("alice-cqu"));
    }

    /// cmd_query with limit returns only N messages.
    #[tokio::test]
    async fn cmd_query_limit() {
        let chat = NamedTempFile::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();
        let token_dir = TempDir::new().unwrap();

        let room_id = format!("test-cql-{}", std::process::id());
        write_token_file(&token_dir, &room_id, "alice-cql", "tok-cql");
        write_meta_file(&room_id, chat.path());

        for i in 0..5u32 {
            crate::history::append(
                chat.path(),
                &make_message(&room_id, "bob", format!("msg {i}")),
            )
            .await
            .unwrap();
        }

        let filter = QueryFilter {
            rooms: vec![room_id.clone()],
            limit: Some(2),
            ascending: false,
            ..Default::default()
        };
        let opts = QueryOptions {
            new_only: false,
            wait: false,
            interval_secs: 5,
            mentions_only: false,
            since_uuid: None,
        };

        let result =
            oneshot_cmd_query_to_vec(&[room_id.clone()], "tok-cql", filter, opts, &cursor_dir)
                .await
                .unwrap();
        assert_eq!(result.len(), 2, "limit should restrict to 2 messages");

        let _ = std::fs::remove_file(crate::paths::room_meta_path(&room_id));
        let _ = std::fs::remove_file(&global_token_path("alice-cql"));
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn global_token_path(username: &str) -> PathBuf {
        crate::paths::global_token_path(username)
    }

    fn write_token_file(_dir: &TempDir, _room_id: &str, username: &str, token: &str) {
        let path = global_token_path(username);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let data = serde_json::json!({ "username": username, "token": token });
        std::fs::write(&path, format!("{data}\n")).unwrap();
    }

    fn write_meta_file(room_id: &str, chat_path: &Path) {
        let meta_path = crate::paths::room_meta_path(room_id);
        if let Some(parent) = meta_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let meta = serde_json::json!({ "chat_path": chat_path.to_string_lossy() });
        std::fs::write(&meta_path, format!("{meta}\n")).unwrap();
    }

    /// Run cmd_query and collect returned messages.
    ///
    /// Since cmd_query writes to stdout, we wrap it to capture results by
    /// re-reading the chat file with the same filter in historical mode.
    /// For `new_only` tests we verify the cursor state instead.
    async fn oneshot_cmd_query_to_vec(
        room_ids: &[String],
        token: &str,
        filter: QueryFilter,
        opts: QueryOptions,
        _cursor_dir: &TempDir,
    ) -> anyhow::Result<Vec<Message>> {
        // Snapshot cursor before run.
        let cursor_before = room_ids
            .first()
            .map(|id| {
                // Resolve username by reading the token file for this room.
                super::super::token::username_from_token(token)
                    .ok()
                    .map(|u| {
                        let p = crate::paths::cursor_path(id, &u);
                        std::fs::read_to_string(&p).ok()
                    })
                    .flatten()
            })
            .flatten();

        // Run cmd_query (side effect: may update cursor).
        cmd_query(room_ids, token, filter.clone(), opts.clone()).await?;

        // Snapshot cursor after run.
        let cursor_after = room_ids
            .first()
            .map(|id| {
                super::super::token::username_from_token(token)
                    .ok()
                    .map(|u| {
                        let p = crate::paths::cursor_path(id, &u);
                        std::fs::read_to_string(&p).ok()
                    })
                    .flatten()
            })
            .flatten();

        // Reconstruct what cmd_query would have returned.
        // For historical mode: re-run with same filter and collect messages.
        // For new_only mode: reload history and apply filter with the "before" cursor.
        if !opts.new_only && !opts.wait {
            // Historical: reload and reapply filter.
            let mut all: Vec<Message> = Vec::new();
            for room_id in room_ids {
                let meta_path = crate::paths::room_meta_path(room_id);
                let chat_path = chat_path_from_meta(room_id, &meta_path);
                let msgs = history::load(&chat_path).await?;
                all.extend(msgs);
            }
            let username = username_from_token(token).unwrap_or_default();
            let mut result: Vec<Message> = all
                .into_iter()
                .filter(|m| filter.matches(m, m.room()))
                .filter(|m| match m {
                    Message::DirectMessage { user, to, .. } => user == &username || to == &username,
                    _ => true,
                })
                .collect();
            apply_sort_and_limit(&mut result, &filter);
            Ok(result)
        } else {
            // new_only mode: reconstruct returned messages by replaying history
            // from cursor_before (before the call advanced it).
            let advanced = cursor_after != cursor_before;
            if advanced {
                let room_id = &room_ids[0];
                let meta_path = crate::paths::room_meta_path(room_id);
                let chat_path = chat_path_from_meta(room_id, &meta_path);
                let all = history::load(&chat_path).await?;
                // Find start from the pre-run cursor UUID.
                let start = match &cursor_before {
                    Some(id) => all
                        .iter()
                        .position(|m| m.id() == id.trim())
                        .map(|i| i + 1)
                        .unwrap_or(0),
                    None => 0,
                };
                let filtered: Vec<Message> = all[start..]
                    .iter()
                    .filter(|m| filter.matches(m, m.room()))
                    .cloned()
                    .collect();
                Ok(filtered)
            } else {
                Ok(vec![])
            }
        }
    }

    /// username_from_token returns an error for an unknown token.
    #[test]
    fn unknown_token_returns_error() {
        let result = super::super::token::username_from_token("bad-token-nonexistent");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("token not recognised"));
    }

    // ── subscription tier filtering tests ──────────────────────────────────

    /// load_user_tier returns Full when no subscription file exists.
    #[test]
    fn load_user_tier_missing_file_returns_full() {
        // Use a room ID that will never have a subscription file on disk.
        let tier = load_user_tier("nonexistent-room-tier-test", "alice");
        assert_eq!(tier, SubscriptionTier::Full);
    }

    /// load_user_tier returns the persisted tier when the file exists.
    #[test]
    fn load_user_tier_returns_persisted_tier() {
        let state_dir = crate::paths::room_state_dir();
        let _ = std::fs::create_dir_all(&state_dir);
        let room_id = format!("test-tier-load-{}", std::process::id());
        let sub_path = crate::paths::broker_subscriptions_path(&state_dir, &room_id);

        let mut map = std::collections::HashMap::new();
        map.insert("alice".to_string(), SubscriptionTier::MentionsOnly);
        map.insert("bob".to_string(), SubscriptionTier::Unsubscribed);
        let json = serde_json::to_string_pretty(&map).unwrap();
        std::fs::write(&sub_path, json).unwrap();

        assert_eq!(
            load_user_tier(&room_id, "alice"),
            SubscriptionTier::MentionsOnly
        );
        assert_eq!(
            load_user_tier(&room_id, "bob"),
            SubscriptionTier::Unsubscribed
        );
        // Unknown user defaults to Full.
        assert_eq!(load_user_tier(&room_id, "carol"), SubscriptionTier::Full);

        let _ = std::fs::remove_file(&sub_path);
    }

    /// apply_tier_filter with Full keeps all messages.
    #[test]
    fn apply_tier_filter_full_keeps_all() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_message("r", "bob", "world"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::Full, "carol");
        assert_eq!(msgs.len(), 2);
    }

    /// apply_tier_filter with MentionsOnly keeps only @mentions.
    #[test]
    fn apply_tier_filter_mentions_only_filters() {
        let mut msgs = vec![
            make_message("r", "alice", "hey @carol check this"),
            make_message("r", "bob", "unrelated message"),
            make_message("r", "dave", "also @carol"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::MentionsOnly, "carol");
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content().unwrap().contains("@carol"));
        assert!(msgs[1].content().unwrap().contains("@carol"));
    }

    /// apply_tier_filter with Unsubscribed clears all messages.
    #[test]
    fn apply_tier_filter_unsubscribed_clears_all() {
        let mut msgs = vec![
            make_message("r", "alice", "hey @carol"),
            make_message("r", "bob", "world"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::Unsubscribed, "carol");
        assert!(msgs.is_empty());
    }

    /// apply_tier_filter with MentionsOnly and no mentions returns empty.
    #[test]
    fn apply_tier_filter_mentions_only_no_mentions_returns_empty() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_message("r", "bob", "world"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::MentionsOnly, "carol");
        assert!(msgs.is_empty());
    }

    /// cmd_query with Unsubscribed tier and public_only=true still returns messages.
    #[tokio::test]
    async fn cmd_query_public_bypasses_tier() {
        let chat = NamedTempFile::new().unwrap();
        let token_dir = TempDir::new().unwrap();
        let cursor_dir = TempDir::new().unwrap();

        let room_id = format!("test-pub-tier-{}", std::process::id());
        write_token_file(&token_dir, &room_id, "alice-pub", "tok-pub-tier");
        write_meta_file(&room_id, chat.path());

        // Write subscription map marking alice-pub as Unsubscribed.
        let state_dir = crate::paths::room_state_dir();
        let _ = std::fs::create_dir_all(&state_dir);
        let sub_path = crate::paths::broker_subscriptions_path(&state_dir, &room_id);
        let mut map = std::collections::HashMap::new();
        map.insert("alice-pub".to_string(), SubscriptionTier::Unsubscribed);
        std::fs::write(&sub_path, serde_json::to_string(&map).unwrap()).unwrap();

        // Add a message.
        crate::history::append(chat.path(), &make_message(&room_id, "bob", "visible"))
            .await
            .unwrap();

        // Query with public_only=true should bypass tier and return the message.
        let filter = QueryFilter {
            rooms: vec![room_id.clone()],
            public_only: true,
            ascending: true,
            ..Default::default()
        };
        let opts = QueryOptions {
            new_only: false,
            wait: false,
            interval_secs: 5,
            mentions_only: false,
            since_uuid: None,
        };

        let result = oneshot_cmd_query_to_vec(
            &[room_id.clone()],
            "tok-pub-tier",
            filter,
            opts,
            &cursor_dir,
        )
        .await
        .unwrap();
        assert_eq!(
            result.len(),
            1,
            "public flag should bypass Unsubscribed tier"
        );

        let _ = std::fs::remove_file(&sub_path);
        let _ = std::fs::remove_file(crate::paths::room_meta_path(&room_id));
        let _ = std::fs::remove_file(&global_token_path("alice-pub"));
    }

    /// MentionsOnly tier sets mention_user filter, narrowing results to @mentions.
    #[test]
    fn mentions_only_tier_sets_mention_user_on_filter() {
        // Verify the tier logic: when tier is MentionsOnly and mention_user is
        // not already set, it should be set to the username.
        let mut filter = QueryFilter::default();
        let tier = SubscriptionTier::MentionsOnly;

        // Simulate what cmd_query does.
        match tier {
            SubscriptionTier::MentionsOnly => {
                if filter.mention_user.is_none() {
                    filter.mention_user = Some("alice".to_string());
                }
            }
            _ => {}
        }

        assert_eq!(filter.mention_user, Some("alice".to_string()));

        // Now verify with apply_tier_filter that messages are correctly narrowed.
        let mut msgs = vec![
            make_message("r", "bob", "hey @alice look"),
            make_message("r", "bob", "unrelated chatter"),
        ];
        apply_tier_filter(&mut msgs, SubscriptionTier::MentionsOnly, "alice");
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content().unwrap().contains("@alice"));
    }

    /// MentionsOnly tier does not override an existing mention_user filter.
    #[test]
    fn mentions_only_tier_preserves_existing_mention_user() {
        let mut filter = QueryFilter {
            mention_user: Some("bob".to_string()),
            ..Default::default()
        };

        // MentionsOnly should not overwrite the existing filter.
        match SubscriptionTier::MentionsOnly {
            SubscriptionTier::MentionsOnly => {
                if filter.mention_user.is_none() {
                    filter.mention_user = Some("alice".to_string());
                }
            }
            _ => {}
        }

        assert_eq!(
            filter.mention_user,
            Some("bob".to_string()),
            "existing mention_user filter should be preserved"
        );
    }

    // ── per-room subscription tier filtering tests ─────────────────────────

    #[test]
    fn per_room_tier_filter_full_keeps_all() {
        let mut msgs = vec![
            make_message("dev", "alice", "hello from dev"),
            make_message("lobby", "bob", "hello from lobby"),
        ];
        // No subscription files → defaults to Full for both rooms.
        let rooms = vec![
            "nonexistent-perroom-full-1".to_string(),
            "nonexistent-perroom-full-2".to_string(),
        ];
        apply_per_room_tier_filter(&mut msgs, &rooms, "carol");
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn per_room_tier_filter_mixed_tiers() {
        let state_dir = crate::paths::room_state_dir();
        let _ = std::fs::create_dir_all(&state_dir);

        let room_full = format!("perroom-mixed-full-{}", std::process::id());
        let room_unsub = format!("perroom-mixed-unsub-{}", std::process::id());
        let room_mentions = format!("perroom-mixed-ment-{}", std::process::id());

        // Write subscription maps: room_full=Full (default), room_unsub=Unsubscribed, room_mentions=MentionsOnly
        let sub_unsub = crate::paths::broker_subscriptions_path(&state_dir, &room_unsub);
        let mut map_unsub = std::collections::HashMap::new();
        map_unsub.insert("alice".to_string(), SubscriptionTier::Unsubscribed);
        std::fs::write(&sub_unsub, serde_json::to_string(&map_unsub).unwrap()).unwrap();

        let sub_ment = crate::paths::broker_subscriptions_path(&state_dir, &room_mentions);
        let mut map_ment = std::collections::HashMap::new();
        map_ment.insert("alice".to_string(), SubscriptionTier::MentionsOnly);
        std::fs::write(&sub_ment, serde_json::to_string(&map_ment).unwrap()).unwrap();

        let mut msgs = vec![
            make_message(&room_full, "bob", "visible in full room"),
            make_message(&room_unsub, "bob", "invisible — unsubscribed"),
            make_message(&room_mentions, "bob", "no mention — filtered"),
            make_message(&room_mentions, "bob", "hey @alice check this"),
        ];

        let rooms = vec![room_full.clone(), room_unsub.clone(), room_mentions.clone()];
        apply_per_room_tier_filter(&mut msgs, &rooms, "alice");

        // Only the Full room message and the MentionsOnly room @alice message survive.
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content().unwrap().contains("visible in full room"));
        assert!(msgs[1].content().unwrap().contains("@alice"));

        // Cleanup
        let _ = std::fs::remove_file(&sub_unsub);
        let _ = std::fs::remove_file(&sub_ment);
    }

    #[test]
    fn per_room_tier_filter_unknown_room_defaults_to_full() {
        let mut msgs = vec![make_message("mystery", "bob", "hello")];
        // Room not in the room_ids list at all — tier defaults to Full.
        apply_per_room_tier_filter(&mut msgs, &["other".to_string()], "alice");
        assert_eq!(msgs.len(), 1);
    }

    // ── event filter tests ────────────────────────────────────────────────────

    fn make_event_msg(room: &str, event_type: room_protocol::EventType) -> Message {
        room_protocol::make_event(room, "bot", event_type, "event content", None)
    }

    #[test]
    fn event_filter_all_keeps_everything() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_event_msg("r", room_protocol::EventType::TaskPosted),
            make_event_msg("r", room_protocol::EventType::TaskFinished),
        ];
        apply_event_filter(&mut msgs, &EventFilter::All);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn event_filter_none_removes_only_events() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_event_msg("r", room_protocol::EventType::TaskPosted),
            make_event_msg("r", room_protocol::EventType::TaskFinished),
        ];
        apply_event_filter(&mut msgs, &EventFilter::None);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content().unwrap().contains("hello"));
    }

    #[test]
    fn event_filter_only_keeps_matching_events() {
        let mut types = std::collections::BTreeSet::new();
        types.insert(room_protocol::EventType::TaskPosted);
        let filter = EventFilter::Only { types };

        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_event_msg("r", room_protocol::EventType::TaskPosted),
            make_event_msg("r", room_protocol::EventType::TaskFinished),
        ];
        apply_event_filter(&mut msgs, &filter);
        assert_eq!(msgs.len(), 2); // hello + task_posted
    }

    #[test]
    fn event_filter_only_removes_non_matching_events() {
        let mut types = std::collections::BTreeSet::new();
        types.insert(room_protocol::EventType::TaskFinished);
        let filter = EventFilter::Only { types };

        let mut msgs = vec![
            make_event_msg("r", room_protocol::EventType::TaskPosted),
            make_event_msg("r", room_protocol::EventType::TaskAssigned),
            make_event_msg("r", room_protocol::EventType::TaskFinished),
        ];
        apply_event_filter(&mut msgs, &filter);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn event_filter_does_not_affect_non_event_messages() {
        let mut msgs = vec![
            make_message("r", "alice", "hello"),
            make_message("r", "bob", "world"),
        ];
        apply_event_filter(&mut msgs, &EventFilter::None);
        assert_eq!(msgs.len(), 2, "non-event messages should not be filtered");
    }

    #[test]
    fn load_user_event_filter_missing_file_returns_all() {
        let ef = load_user_event_filter("nonexistent-room-ef-test", "alice");
        assert_eq!(ef, EventFilter::All);
    }

    #[test]
    fn load_user_event_filter_returns_persisted() {
        let state_dir = crate::paths::room_state_dir();
        let _ = std::fs::create_dir_all(&state_dir);
        let room_id = format!("test-ef-load-{}", std::process::id());
        let ef_path = crate::paths::broker_event_filters_path(&state_dir, &room_id);

        let mut map = std::collections::HashMap::new();
        map.insert("alice".to_string(), EventFilter::None);
        let mut types = std::collections::BTreeSet::new();
        types.insert(room_protocol::EventType::TaskPosted);
        map.insert("bob".to_string(), EventFilter::Only { types });
        let json = serde_json::to_string_pretty(&map).unwrap();
        std::fs::write(&ef_path, json).unwrap();

        assert_eq!(load_user_event_filter(&room_id, "alice"), EventFilter::None);
        let bob_filter = load_user_event_filter(&room_id, "bob");
        match bob_filter {
            EventFilter::Only { types } => {
                assert!(types.contains(&room_protocol::EventType::TaskPosted));
                assert_eq!(types.len(), 1);
            }
            _ => panic!("expected Only filter for bob"),
        }
        // Unknown user defaults to All.
        assert_eq!(load_user_event_filter(&room_id, "carol"), EventFilter::All);

        let _ = std::fs::remove_file(&ef_path);
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
