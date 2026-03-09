use std::path::{Path, PathBuf};

use room_protocol::SubscriptionTier;
use tokio::net::UnixStream;
use tokio::time::{timeout, Duration};

use crate::broker::commands::load_subscription_map;

/// Information about a discovered room with a live broker.
#[derive(serde::Serialize)]
struct RoomInfo {
    room: String,
    socket: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_path: Option<String>,
}

/// Discover all daemon-managed rooms by scanning for `.meta` files in the
/// runtime directory. Returns room IDs sorted alphabetically.
///
/// This returns ALL daemon rooms regardless of membership. Use
/// [`discover_joined_rooms`] to filter to rooms the user has joined.
pub fn discover_daemon_rooms() -> Vec<String> {
    discover_daemon_rooms_in(&crate::paths::room_runtime_dir())
}

/// Discover daemon rooms the user is subscribed to.
///
/// Scans for `.meta` files, then filters to rooms where the user has
/// a subscription entry that is not `Unsubscribed`. Returns room IDs
/// sorted alphabetically.
pub fn discover_joined_rooms(username: &str) -> Vec<String> {
    discover_joined_rooms_in(
        &crate::paths::room_runtime_dir(),
        &crate::paths::room_state_dir(),
        username,
    )
}

/// Testable inner: discover rooms from `runtime_dir` where `username` has an
/// active subscription in `state_dir`.
fn discover_joined_rooms_in(runtime_dir: &Path, state_dir: &Path, username: &str) -> Vec<String> {
    discover_daemon_rooms_in(runtime_dir)
        .into_iter()
        .filter(|room_id| {
            let sub_path = crate::paths::broker_subscriptions_path(state_dir, room_id);
            let map = load_subscription_map(&sub_path);
            matches!(
                map.get(username),
                Some(SubscriptionTier::Full) | Some(SubscriptionTier::MentionsOnly)
            )
        })
        .collect()
}

/// Scan `dir` for `room-*.meta` files and return room IDs sorted alphabetically.
fn discover_daemon_rooms_in(dir: &Path) -> Vec<String> {
    let mut rooms = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(room_id) = name_str
                .strip_prefix("room-")
                .and_then(|s| s.strip_suffix(".meta"))
            {
                rooms.push(room_id.to_string());
            }
        }
    }
    rooms.sort();
    rooms
}

/// Scan the platform runtime directory for `room-*.sock` files, verify each
/// broker is alive via a short connect attempt, and print one NDJSON line per
/// active room.
pub async fn cmd_list() -> anyhow::Result<()> {
    let rooms = discover_rooms(&crate::paths::room_runtime_dir()).await?;

    for info in &rooms {
        println!("{}", serde_json::to_string(info)?);
    }

    Ok(())
}

/// Scan `dir` for `room-*.sock` files and return info for each live broker.
async fn discover_rooms(dir: &Path) -> anyhow::Result<Vec<RoomInfo>> {
    let mut rooms: Vec<RoomInfo> = Vec::new();

    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        let Some(room_id) = name_str
            .strip_prefix("room-")
            .and_then(|s| s.strip_suffix(".sock"))
        else {
            continue;
        };

        let socket_path = entry.path();
        // Short connect timeout to filter stale sockets without blocking.
        let alive = timeout(
            Duration::from_millis(200),
            UnixStream::connect(&socket_path),
        )
        .await
        .is_ok_and(|r| r.is_ok());

        if !alive {
            continue;
        }

        let meta_path = dir.join(format!("room-{room_id}.meta"));
        let chat_path = read_chat_path_from_meta(&meta_path);

        rooms.push(RoomInfo {
            room: room_id.to_string(),
            socket: socket_path,
            chat_path,
        });
    }

    rooms.sort_by(|a, b| a.room.cmp(&b.room));
    Ok(rooms)
}

fn read_chat_path_from_meta(meta_path: &Path) -> Option<String> {
    let data = std::fs::read_to_string(meta_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v["chat_path"].as_str().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_chat_path_from_valid_meta() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("room-test.meta");
        std::fs::write(&meta, r#"{"chat_path":"/tmp/test.ndjson"}"#).unwrap();
        assert_eq!(
            read_chat_path_from_meta(&meta),
            Some("/tmp/test.ndjson".to_string())
        );
    }

    #[test]
    fn read_chat_path_missing_file_returns_none() {
        let path = Path::new("/tmp/nonexistent-meta-file-bumblebee-test.meta");
        assert_eq!(read_chat_path_from_meta(path), None);
    }

    #[test]
    fn read_chat_path_invalid_json_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("room-bad.meta");
        std::fs::write(&meta, "not json").unwrap();
        assert_eq!(read_chat_path_from_meta(&meta), None);
    }

    #[test]
    fn read_chat_path_missing_key_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("room-nokey.meta");
        std::fs::write(&meta, r#"{"other":"value"}"#).unwrap();
        assert_eq!(read_chat_path_from_meta(&meta), None);
    }

    #[tokio::test]
    async fn discover_rooms_empty_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let rooms = discover_rooms(dir.path()).await.unwrap();
        assert!(rooms.is_empty());
    }

    #[tokio::test]
    async fn discover_rooms_skips_non_socket_files() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file that matches the naming pattern but isn't a real socket.
        std::fs::write(dir.path().join("room-fake.sock"), "").unwrap();
        let rooms = discover_rooms(dir.path()).await.unwrap();
        assert!(
            rooms.is_empty(),
            "regular file should not be listed as live"
        );
    }

    #[tokio::test]
    async fn discover_rooms_with_live_broker() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("room-testroom.sock");

        // Start a listener to simulate a live broker.
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();

        // Write a meta file so chat_path is included.
        let meta_path = dir.path().join("room-testroom.meta");
        std::fs::write(&meta_path, r#"{"chat_path":"/tmp/testroom.ndjson"}"#).unwrap();

        // Spawn a task to accept (and immediately drop) the probe connection.
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let rooms = discover_rooms(dir.path()).await.unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room, "testroom");
        assert_eq!(rooms[0].chat_path.as_deref(), Some("/tmp/testroom.ndjson"));
    }

    #[tokio::test]
    async fn discover_rooms_without_meta_omits_chat_path() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("room-nometa.sock");

        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let rooms = discover_rooms(dir.path()).await.unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room, "nometa");
        assert!(rooms[0].chat_path.is_none());
    }

    #[tokio::test]
    async fn discover_rooms_sorts_alphabetically() {
        let dir = tempfile::tempdir().unwrap();

        let mut listeners = Vec::new();
        for name in ["room-zebra.sock", "room-alpha.sock", "room-mid.sock"] {
            let path = dir.path().join(name);
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            listeners.push(listener);
        }

        // Accept probe connections from all listeners.
        for listener in listeners {
            tokio::spawn(async move {
                loop {
                    let _ = listener.accept().await;
                }
            });
        }

        let rooms = discover_rooms(dir.path()).await.unwrap();
        let names: Vec<&str> = rooms.iter().map(|r| r.room.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zebra"]);
    }

    // ── discover_daemon_rooms_in ──────────────────────────────────────────

    #[test]
    fn discover_daemon_rooms_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_daemon_rooms_in(dir.path()).is_empty());
    }

    #[test]
    fn discover_daemon_rooms_finds_meta_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("room-dev.meta"),
            r#"{"chat_path":"/tmp/dev.chat"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("room-lobby.meta"),
            r#"{"chat_path":"/tmp/lobby.chat"}"#,
        )
        .unwrap();
        // Non-meta files should be ignored.
        std::fs::write(dir.path().join("room-other.sock"), "").unwrap();
        std::fs::write(dir.path().join("roomd.sock"), "").unwrap();

        let rooms = discover_daemon_rooms_in(dir.path());
        assert_eq!(rooms, vec!["dev", "lobby"]);
    }

    #[test]
    fn discover_daemon_rooms_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["room-zebra.meta", "room-alpha.meta", "room-mid.meta"] {
            std::fs::write(dir.path().join(name), "{}").unwrap();
        }
        let rooms = discover_daemon_rooms_in(dir.path());
        assert_eq!(rooms, vec!["alpha", "mid", "zebra"]);
    }

    // ── discover_joined_rooms_in ──────────────────────────────────────────

    /// Helper: write a subscription file for a room.
    fn write_sub_file(
        state_dir: &Path,
        room_id: &str,
        subs: &std::collections::HashMap<String, SubscriptionTier>,
    ) {
        let path = state_dir.join(format!("{room_id}.subscriptions"));
        let json = serde_json::to_string(subs).unwrap();
        std::fs::write(path, json).unwrap();
    }

    #[test]
    fn joined_rooms_returns_subscribed_rooms() {
        let runtime = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();

        // Create meta files for two rooms.
        std::fs::write(runtime.path().join("room-alpha.meta"), "{}").unwrap();
        std::fs::write(runtime.path().join("room-beta.meta"), "{}").unwrap();

        // Alice subscribed to alpha (Full), not in beta's subscription file.
        let mut subs = std::collections::HashMap::new();
        subs.insert("alice".to_owned(), SubscriptionTier::Full);
        write_sub_file(state.path(), "alpha", &subs);

        let rooms = discover_joined_rooms_in(runtime.path(), state.path(), "alice");
        assert_eq!(rooms, vec!["alpha"]);
    }

    #[test]
    fn joined_rooms_includes_mentions_only() {
        let runtime = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();

        std::fs::write(runtime.path().join("room-dev.meta"), "{}").unwrap();

        let mut subs = std::collections::HashMap::new();
        subs.insert("bob".to_owned(), SubscriptionTier::MentionsOnly);
        write_sub_file(state.path(), "dev", &subs);

        let rooms = discover_joined_rooms_in(runtime.path(), state.path(), "bob");
        assert_eq!(rooms, vec!["dev"]);
    }

    #[test]
    fn joined_rooms_excludes_unsubscribed() {
        let runtime = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();

        std::fs::write(runtime.path().join("room-dev.meta"), "{}").unwrap();

        let mut subs = std::collections::HashMap::new();
        subs.insert("carol".to_owned(), SubscriptionTier::Unsubscribed);
        write_sub_file(state.path(), "dev", &subs);

        let rooms = discover_joined_rooms_in(runtime.path(), state.path(), "carol");
        assert!(rooms.is_empty());
    }

    #[test]
    fn joined_rooms_excludes_rooms_without_subscription_file() {
        let runtime = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();

        std::fs::write(runtime.path().join("room-lonely.meta"), "{}").unwrap();
        // No subscription file written for "lonely".

        let rooms = discover_joined_rooms_in(runtime.path(), state.path(), "alice");
        assert!(rooms.is_empty());
    }

    #[test]
    fn joined_rooms_multiple_rooms_sorted() {
        let runtime = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();

        for name in ["room-zebra.meta", "room-alpha.meta", "room-mid.meta"] {
            std::fs::write(runtime.path().join(name), "{}").unwrap();
        }

        // Alice in zebra (Full), alpha (MentionsOnly), unsubscribed from mid.
        for (room, tier) in [
            ("zebra", SubscriptionTier::Full),
            ("alpha", SubscriptionTier::MentionsOnly),
            ("mid", SubscriptionTier::Unsubscribed),
        ] {
            let mut subs = std::collections::HashMap::new();
            subs.insert("alice".to_owned(), tier);
            write_sub_file(state.path(), room, &subs);
        }

        let rooms = discover_joined_rooms_in(runtime.path(), state.path(), "alice");
        assert_eq!(rooms, vec!["alpha", "zebra"]);
    }
}
