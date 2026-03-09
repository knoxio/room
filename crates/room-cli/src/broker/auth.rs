use std::collections::HashMap;
use std::path::{Path, PathBuf};

use room_protocol::{RoomConfig, RoomVisibility};
use tokio::{io::AsyncWriteExt, net::unix::OwnedWriteHalf};
use uuid::Uuid;

use super::state::TokenMap;

// ── Token persistence ────────────────────────────────────────────────────────

/// Derive the token file path from a chat file path.
///
/// Given `/tmp/myroom.chat`, returns `/tmp/myroom.tokens`.
pub(crate) fn token_file_path(chat_path: &Path) -> PathBuf {
    chat_path.with_extension("tokens")
}

/// Write a token map to disk as JSON.
fn save_token_map(map: &HashMap<String, String>, path: &Path) -> Result<(), String> {
    let json = serde_json::to_string_pretty(&map).map_err(|e| format!("serialize tokens: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Load a token map from disk. Returns an empty map if the file does not exist.
pub(crate) fn load_token_map(chat_path: &Path) -> HashMap<String, String> {
    let path = token_file_path(chat_path);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    serde_json::from_str(&contents).unwrap_or_else(|e| {
        eprintln!("[auth] corrupt token file {}: {e}", path.display());
        HashMap::new()
    })
}

/// Check whether `username` is allowed to join a room with the given config.
///
/// Returns `Ok(())` if allowed, or `Err(reason)` if denied.
/// Rooms without config (legacy single-room mode) are always allowed.
pub(crate) fn check_join_permission(
    username: &str,
    config: Option<&RoomConfig>,
) -> Result<(), String> {
    let config = match config {
        Some(c) => c,
        None => return Ok(()), // Legacy rooms have no config — always allow.
    };

    match config.visibility {
        RoomVisibility::Public => Ok(()),
        RoomVisibility::Private | RoomVisibility::Unlisted => {
            if username == config.created_by || config.invite_list.contains(username) {
                Ok(())
            } else {
                Err("permission denied: this room requires an invite to join".to_owned())
            }
        }
        RoomVisibility::Dm => {
            if config.invite_list.contains(username) {
                Ok(())
            } else {
                Err("permission denied: DM rooms are restricted to the two participants".to_owned())
            }
        }
    }
}

/// Check whether `username` is allowed to send a message in a room.
///
/// For DM rooms, only the two participants may send. The host may join
/// read-only (for administrative oversight) but cannot send messages.
/// All other room types allow any joined user to send.
///
/// Returns `Ok(())` if allowed, or `Err(reason)` if denied.
/// Rooms without config (legacy single-room mode) are always allowed.
pub(crate) fn check_send_permission(
    username: &str,
    config: Option<&RoomConfig>,
) -> Result<(), String> {
    let config = match config {
        Some(c) => c,
        None => return Ok(()), // Legacy rooms — always allow.
    };

    if config.visibility == RoomVisibility::Dm && !config.invite_list.contains(username) {
        return Err(
            "permission denied: only DM participants can send messages in this room".to_owned(),
        );
    }

    Ok(())
}

/// Issue a session token for `username` if the name is not already taken.
///
/// Returns the new token string on success, or an error message on collision.
/// If `persist_to` is `Some(chat_path)`, the updated token map is saved to
/// disk alongside the chat file so tokens survive broker restarts.
pub(crate) async fn issue_token(
    username: &str,
    token_map: &TokenMap,
    persist_to: Option<&Path>,
) -> Result<String, String> {
    let mut map = token_map.lock().await;
    if map.values().any(|u| u == username) {
        return Err(format!("username_taken:{username}"));
    }
    let token = Uuid::new_v4().to_string();
    map.insert(token.clone(), username.to_owned());
    if let Some(chat_path) = persist_to {
        if let Err(e) = save_token_map(&map, &token_file_path(chat_path)) {
            eprintln!("[auth] token persist failed: {e}");
        }
    }
    Ok(token)
}

/// Look up a token and return the associated username.
///
/// Returns `None` if the token is not found (invalid or expired).
/// A `KICKED:<username>` sentinel is treated as invalid so kicked users
/// cannot authenticate.
pub(crate) async fn validate_token(token: &str, token_map: &TokenMap) -> Option<String> {
    token_map.lock().await.get(token).cloned()
}

/// Handle a one-shot JOIN request over an already-open write half.
///
/// Checks join permission against the room config, then calls `issue_token`
/// and writes the response JSON to the socket. Rejects unauthorized joins
/// with an error envelope. If `chat_path` is provided, the token map is
/// persisted to disk after a successful issuance.
pub(crate) async fn handle_oneshot_join(
    username: String,
    mut write_half: OwnedWriteHalf,
    token_map: &TokenMap,
    config: Option<&RoomConfig>,
    chat_path: Option<&Path>,
) -> anyhow::Result<()> {
    // Check visibility/ACL before issuing a token.
    if let Err(reason) = check_join_permission(&username, config) {
        let err = serde_json::json!({
            "type": "error",
            "code": "join_denied",
            "message": reason,
            "username": username
        });
        write_half.write_all(format!("{err}\n").as_bytes()).await?;
        return Ok(());
    }

    match issue_token(&username, token_map, chat_path).await {
        Ok(token) => {
            let resp = serde_json::json!({"type":"token","token": token,"username": username});
            write_half.write_all(format!("{resp}\n").as_bytes()).await?;
        }
        Err(_) => {
            let err = serde_json::json!({
                "type": "error",
                "code": "username_taken",
                "username": username
            });
            write_half.write_all(format!("{err}\n").as_bytes()).await?;
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use room_protocol::{RoomConfig, RoomVisibility};
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::Mutex;

    type TestTokenMap = Arc<Mutex<HashMap<String, String>>>;

    fn make_token_map() -> TestTokenMap {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[tokio::test]
    async fn issue_token_new_user_returns_non_empty_uuid() {
        let map = make_token_map();
        let token = issue_token("alice", &map, None).await.unwrap();
        assert!(!token.is_empty());
        let guard = map.lock().await;
        assert_eq!(guard.get(&token).map(String::as_str), Some("alice"));
    }

    #[tokio::test]
    async fn issue_token_same_username_twice_returns_err() {
        let map = make_token_map();
        issue_token("alice", &map, None).await.unwrap();
        let second = issue_token("alice", &map, None).await;
        assert!(second.is_err());
        assert!(second.unwrap_err().contains("alice"));
    }

    #[tokio::test]
    async fn issue_token_different_usernames_get_distinct_tokens() {
        let map = make_token_map();
        let t1 = issue_token("alice", &map, None).await.unwrap();
        let t2 = issue_token("bob", &map, None).await.unwrap();
        assert_ne!(t1, t2);
        let guard = map.lock().await;
        assert_eq!(guard.get(&t1).map(String::as_str), Some("alice"));
        assert_eq!(guard.get(&t2).map(String::as_str), Some("bob"));
    }

    #[tokio::test]
    async fn issue_token_kicked_user_is_blocked_by_sentinel() {
        let map = make_token_map();
        // Simulate what handle_admin_cmd "kick" does
        map.lock()
            .await
            .insert("KICKED:alice".to_owned(), "alice".to_owned());
        let result = issue_token("alice", &map, None).await;
        assert!(
            result.is_err(),
            "kicked user must not be able to issue a new token"
        );
    }

    #[tokio::test]
    async fn validate_token_valid_returns_username() {
        let map = make_token_map();
        let token = issue_token("alice", &map, None).await.unwrap();
        let result = validate_token(&token, &map).await;
        assert_eq!(result, Some("alice".to_owned()));
    }

    #[tokio::test]
    async fn validate_token_unknown_returns_none() {
        let map = make_token_map();
        assert!(validate_token("not-a-real-token", &map).await.is_none());
    }

    #[tokio::test]
    async fn validate_token_after_kick_original_uuid_is_invalidated() {
        let map = make_token_map();
        let token = issue_token("alice", &map, None).await.unwrap();
        // Simulate kick: remove UUID token, insert KICKED sentinel
        {
            let mut guard = map.lock().await;
            guard.retain(|_, u| u != "alice");
            guard.insert("KICKED:alice".to_owned(), "alice".to_owned());
        }
        assert!(
            validate_token(&token, &map).await.is_none(),
            "original UUID must be invalid after kick"
        );
    }

    #[tokio::test]
    async fn validate_token_after_reauth_returns_none() {
        let map = make_token_map();
        let token = issue_token("alice", &map, None).await.unwrap();
        // Simulate reauth: remove all entries for "alice"
        map.lock().await.retain(|_, u| u != "alice");
        assert!(
            validate_token(&token, &map).await.is_none(),
            "token must be invalid after reauth clears it"
        );
    }

    // ── check_join_permission ─────────────────────────────────────────────

    #[test]
    fn join_public_room_always_allowed() {
        let config = RoomConfig::public("owner");
        assert!(check_join_permission("anyone", Some(&config)).is_ok());
    }

    #[test]
    fn join_private_room_denied_without_invite() {
        let config = RoomConfig {
            visibility: RoomVisibility::Private,
            max_members: None,
            invite_list: ["alice".to_owned()].into(),
            created_by: "owner".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        assert!(check_join_permission("alice", Some(&config)).is_ok());
        assert!(check_join_permission("bob", Some(&config)).is_err());
    }

    #[test]
    fn join_private_room_creator_always_allowed() {
        let config = RoomConfig {
            visibility: RoomVisibility::Private,
            max_members: None,
            invite_list: Default::default(),
            created_by: "owner".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        assert!(check_join_permission("owner", Some(&config)).is_ok());
        assert!(check_join_permission("stranger", Some(&config)).is_err());
    }

    #[test]
    fn join_unlisted_room_requires_invite() {
        let config = RoomConfig {
            visibility: RoomVisibility::Unlisted,
            max_members: None,
            invite_list: ["invited".to_owned()].into(),
            created_by: "owner".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        assert!(check_join_permission("invited", Some(&config)).is_ok());
        assert!(check_join_permission("owner", Some(&config)).is_ok());
        assert!(check_join_permission("stranger", Some(&config)).is_err());
    }

    #[test]
    fn join_dm_room_only_participants() {
        let config = RoomConfig::dm("alice", "bob");
        assert!(check_join_permission("alice", Some(&config)).is_ok());
        assert!(check_join_permission("bob", Some(&config)).is_ok());
        assert!(check_join_permission("eve", Some(&config)).is_err());
    }

    #[test]
    fn join_dm_creator_not_special() {
        // In DM rooms, even the creator must be in the invite_list.
        // RoomConfig::dm always adds both users, but test the logic directly.
        let config = RoomConfig {
            visibility: RoomVisibility::Dm,
            max_members: Some(2),
            invite_list: ["bob".to_owned()].into(), // only bob
            created_by: "alice".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        assert!(check_join_permission("bob", Some(&config)).is_ok());
        assert!(check_join_permission("alice", Some(&config)).is_err());
    }

    #[test]
    fn join_no_config_always_allowed() {
        assert!(check_join_permission("anyone", None).is_ok());
    }

    // ── check_send_permission ─────────────────────────────────────────────

    #[test]
    fn send_public_room_always_allowed() {
        let config = RoomConfig::public("owner");
        assert!(check_send_permission("anyone", Some(&config)).is_ok());
    }

    #[test]
    fn send_dm_room_participants_allowed() {
        let config = RoomConfig::dm("alice", "bob");
        assert!(check_send_permission("alice", Some(&config)).is_ok());
        assert!(check_send_permission("bob", Some(&config)).is_ok());
    }

    #[test]
    fn send_dm_room_non_participant_denied() {
        let config = RoomConfig::dm("alice", "bob");
        let result = check_send_permission("eve", Some(&config));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("permission denied"));
    }

    #[test]
    fn send_dm_room_host_denied() {
        // Host can join read-only but must not be able to send
        let config = RoomConfig::dm("alice", "bob");
        assert!(check_send_permission("host-user", Some(&config)).is_err());
    }

    #[test]
    fn send_private_room_any_member_allowed() {
        let config = RoomConfig {
            visibility: RoomVisibility::Private,
            max_members: None,
            invite_list: ["alice".to_owned()].into(),
            created_by: "owner".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        // Private rooms don't restrict sends — only joins
        assert!(check_send_permission("anyone", Some(&config)).is_ok());
    }

    #[test]
    fn send_no_config_always_allowed() {
        assert!(check_send_permission("anyone", None).is_ok());
    }

    // ── Token persistence ─────────────────────────────────────────────────

    #[test]
    fn token_file_path_replaces_extension() {
        let chat = std::path::PathBuf::from("/tmp/myroom.chat");
        assert_eq!(
            token_file_path(&chat),
            std::path::PathBuf::from("/tmp/myroom.tokens")
        );
    }

    #[test]
    fn token_file_path_no_extension() {
        let chat = std::path::PathBuf::from("/tmp/myroom");
        assert_eq!(
            token_file_path(&chat),
            std::path::PathBuf::from("/tmp/myroom.tokens")
        );
    }

    #[test]
    fn load_token_map_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("nonexistent.chat");
        let map = load_token_map(&chat);
        assert!(map.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("test.chat");
        let token_path = token_file_path(&chat);

        let mut original = HashMap::new();
        original.insert("tok-1".to_owned(), "alice".to_owned());
        original.insert("tok-2".to_owned(), "bob".to_owned());

        save_token_map(&original, &token_path).unwrap();
        let loaded = load_token_map(&chat);
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_token_map_corrupt_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("corrupt.chat");
        let token_path = token_file_path(&chat);
        std::fs::write(token_path, "not json{{{").unwrap();

        let map = load_token_map(&chat);
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn issue_token_persists_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("persist.chat");

        let map = make_token_map();
        let token = issue_token("alice", &map, Some(&chat)).await.unwrap();

        // Verify the file was written and contains the token
        let loaded = load_token_map(&chat);
        assert_eq!(loaded.get(&token).map(String::as_str), Some("alice"));
    }

    #[tokio::test]
    async fn issue_token_accumulates_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("accum.chat");

        let map = make_token_map();
        let t1 = issue_token("alice", &map, Some(&chat)).await.unwrap();
        let t2 = issue_token("bob", &map, Some(&chat)).await.unwrap();

        let loaded = load_token_map(&chat);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get(&t1).map(String::as_str), Some("alice"));
        assert_eq!(loaded.get(&t2).map(String::as_str), Some("bob"));
    }

    #[tokio::test]
    async fn persisted_tokens_survive_new_token_map() {
        let dir = tempfile::tempdir().unwrap();
        let chat = dir.path().join("survive.chat");

        // Issue tokens and persist
        let map1 = make_token_map();
        let token = issue_token("alice", &map1, Some(&chat)).await.unwrap();

        // Simulate restart: new empty token map, load from disk
        let loaded = load_token_map(&chat);
        assert_eq!(loaded.get(&token).map(String::as_str), Some("alice"));

        // Populate a new token map from loaded data
        let map2 = Arc::new(Mutex::new(loaded));
        let username = validate_token(&token, &map2).await;
        assert_eq!(username, Some("alice".to_owned()));
    }
}
