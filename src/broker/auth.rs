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
