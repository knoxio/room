use tokio::{io::AsyncWriteExt, net::unix::OwnedWriteHalf};
use uuid::Uuid;

use super::state::TokenMap;

/// Issue a session token for `username` if the name is not already taken.
///
/// Returns the new token string on success, or an error message on collision.
/// Inserts the mapping into `token_map`; the caller is responsible for
/// writing the token file to disk.
pub(crate) async fn issue_token(username: &str, token_map: &TokenMap) -> Result<String, String> {
    let mut map = token_map.lock().await;
    if map.values().any(|u| u == username) {
        return Err(format!("username_taken:{username}"));
    }
    let token = Uuid::new_v4().to_string();
    map.insert(token.clone(), username.to_owned());
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
/// Calls `issue_token` and writes the response JSON to the socket.
/// If the username is already taken, writes an error envelope instead.
pub(crate) async fn handle_oneshot_join(
    username: String,
    mut write_half: OwnedWriteHalf,
    token_map: &TokenMap,
) -> anyhow::Result<()> {
    match issue_token(&username, token_map).await {
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
    use super::{issue_token, validate_token};
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::Mutex;

    type TokenMap = Arc<Mutex<HashMap<String, String>>>;

    fn make_token_map() -> TokenMap {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[tokio::test]
    async fn issue_token_new_user_returns_non_empty_uuid() {
        let map = make_token_map();
        let token = issue_token("alice", &map).await.unwrap();
        assert!(!token.is_empty());
        let guard = map.lock().await;
        assert_eq!(guard.get(&token).map(String::as_str), Some("alice"));
    }

    #[tokio::test]
    async fn issue_token_same_username_twice_returns_err() {
        let map = make_token_map();
        issue_token("alice", &map).await.unwrap();
        let second = issue_token("alice", &map).await;
        assert!(second.is_err());
        assert!(second.unwrap_err().contains("alice"));
    }

    #[tokio::test]
    async fn issue_token_different_usernames_get_distinct_tokens() {
        let map = make_token_map();
        let t1 = issue_token("alice", &map).await.unwrap();
        let t2 = issue_token("bob", &map).await.unwrap();
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
        let result = issue_token("alice", &map).await;
        assert!(
            result.is_err(),
            "kicked user must not be able to issue a new token"
        );
    }

    #[tokio::test]
    async fn validate_token_valid_returns_username() {
        let map = make_token_map();
        let token = issue_token("alice", &map).await.unwrap();
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
        let token = issue_token("alice", &map).await.unwrap();
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
        let token = issue_token("alice", &map).await.unwrap();
        // Simulate reauth: remove all entries for "alice"
        map.lock().await.retain(|_, u| u != "alice");
        assert!(
            validate_token(&token, &map).await.is_none(),
            "token must be invalid after reauth clears it"
        );
    }
}
