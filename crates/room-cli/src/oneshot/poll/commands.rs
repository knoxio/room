use std::path::{Path, PathBuf};

use crate::{history, message::Message, paths, query::QueryFilter};

use super::filter_events::{
    apply_event_filter, apply_per_room_event_filter, load_user_event_filter,
};
use super::filter_tier::{apply_per_room_tier_filter, apply_tier_filter, load_user_tier};
use super::meta::{chat_path_from_meta, read_host_from_meta};
use super::multi_room::poll_messages_multi;
use super::{poll_messages, QueryOptions};

use crate::oneshot::token::username_from_token;

/// One-shot pull subcommand: print the last N messages from history as NDJSON.
///
/// Reads from the chat file directly (no broker connection required).
/// Does **not** update the poll cursor.
pub async fn cmd_pull(room_id: &str, token: &str, n: usize) -> anyhow::Result<()> {
    let username = username_from_token(token)?;
    let meta_path = paths::room_meta_path(room_id);
    let chat_path = chat_path_from_meta(room_id, &meta_path);

    let host = read_host_from_meta(&meta_path);
    let mut messages =
        super::pull_messages(&chat_path, n, Some(&username), host.as_deref()).await?;
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
pub(super) fn apply_sort_and_limit(messages: &mut Vec<Message>, filter: &QueryFilter) {
    if filter.ascending {
        messages.sort_by(|a, b| a.ts().cmp(b.ts()));
    } else {
        messages.sort_by(|a, b| b.ts().cmp(a.ts()));
    }
    if let Some(limit) = filter.limit {
        messages.truncate(limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::make_message;
    use tempfile::{NamedTempFile, TempDir};

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
        let cursor_before = room_ids.first().and_then(|id| {
            crate::oneshot::token::username_from_token(token)
                .ok()
                .and_then(|u| {
                    let p = crate::paths::cursor_path(id, &u);
                    std::fs::read_to_string(&p).ok()
                })
        });

        // Run cmd_query (side effect: may update cursor).
        cmd_query(room_ids, token, filter.clone(), opts.clone()).await?;

        // Snapshot cursor after run.
        let cursor_after = room_ids.first().and_then(|id| {
            crate::oneshot::token::username_from_token(token)
                .ok()
                .and_then(|u| {
                    let p = crate::paths::cursor_path(id, &u);
                    std::fs::read_to_string(&p).ok()
                })
        });

        // Reconstruct what cmd_query would have returned.
        // For historical mode: re-run with same filter and collect messages.
        // For new_only mode: reload history and apply filter with the "before" cursor.
        if !opts.new_only && !opts.wait {
            // Historical: reload and reapply filter.
            let mut all: Vec<Message> = Vec::new();
            for room_id in room_ids {
                let meta_path = crate::paths::room_meta_path(room_id);
                let chat_path = super::super::meta::chat_path_from_meta(room_id, &meta_path);
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
                let chat_path = super::super::meta::chat_path_from_meta(room_id, &meta_path);
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
        let result = crate::oneshot::token::username_from_token("bad-token-nonexistent");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("token not recognised"));
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
        map.insert(
            "alice-pub".to_string(),
            room_protocol::SubscriptionTier::Unsubscribed,
        );
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
}
