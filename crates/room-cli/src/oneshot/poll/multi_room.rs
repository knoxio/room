use std::path::Path;

use crate::{message::Message, paths};

use super::meta::read_host_from_meta;
use super::poll_messages;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::make_message;
    use tempfile::NamedTempFile;

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
}
