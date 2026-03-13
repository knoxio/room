//! `RoomService` trait — high-level operations REST handlers need.
//!
//! Decouples `broker/ws/rest.rs` from `RoomState` internals. REST handlers
//! call trait methods instead of reaching into `token_map`, `host_user`,
//! `clients`, `chat_path`, and `seq_counter` directly.
//!
//! The concrete implementation lives on `RoomState` (below). WS handlers
//! continue to use `RoomState` directly for socket lifecycle management.

use crate::message::Message;

use room_protocol::SubscriptionTier;

use super::{
    auth::{check_join_permission, check_send_permission, issue_token, validate_token},
    commands::{route_command, CommandResult},
    fanout::{broadcast_and_persist, dm_and_persist},
    state::RoomState,
};

/// Result of dispatching a message through the command router + broadcast pipeline.
pub(crate) enum DispatchResult {
    /// Command handled internally (broadcast already sent if applicable).
    Handled,
    /// Command produced a JSON reply for the caller.
    Reply(String),
    /// The broker is shutting down (after `/exit`).
    Shutdown,
    /// Message was broadcast or DM'd. Contains the sequenced message.
    Sent(Message),
    /// Send denied (DM privacy or permission check failed).
    SendDenied(String),
}

/// High-level room operations consumed by REST handlers.
///
/// Abstracts away `RoomState` internals so REST endpoints depend on a trait
/// boundary rather than concrete field access. This makes REST handlers
/// testable with mock implementations and clarifies the API surface between
/// the HTTP layer and the broker core.
pub(crate) trait RoomService: Send + Sync {
    /// The room's unique identifier.
    fn room_id(&self) -> &str;

    /// Validate a bearer token, returning the associated username if valid.
    fn validate_token(
        &self,
        token: &str,
    ) -> impl std::future::Future<Output = Option<String>> + Send;

    /// Issue a new token for `username`. Returns the token UUID on success,
    /// or an error string if the username is already taken (or kicked).
    fn issue_token(
        &self,
        username: &str,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send;

    /// Check whether `username` is allowed to join this room.
    fn check_join(&self, username: &str) -> Result<(), String>;

    /// Return the host username, if one has been elected.
    fn host_name(&self) -> impl std::future::Future<Output = Option<String>> + Send;

    /// Number of users currently online (in the status map).
    fn status_count(&self) -> impl std::future::Future<Output = usize> + Send;

    /// Load the full chat history from disk.
    fn load_history(&self) -> impl std::future::Future<Output = Vec<Message>> + Send;

    /// Route a parsed message through the command system and, if it falls
    /// through as a regular message, broadcast or DM it.
    ///
    /// Combines `route_command` + passthrough send logic so REST handlers
    /// don't need to know about `broadcast_and_persist` or `dm_and_persist`.
    fn route_and_dispatch(
        &self,
        msg: Message,
        username: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<DispatchResult>> + Send;
}

// ── RoomState implementation ─────────────────────────────────────────────────

impl RoomService for RoomState {
    fn room_id(&self) -> &str {
        &self.room_id
    }

    async fn validate_token(&self, token: &str) -> Option<String> {
        validate_token(token, &self.auth.token_map).await
    }

    async fn issue_token(&self, username: &str) -> Result<String, String> {
        let token = issue_token(
            username,
            &self.auth.token_map,
            Some(&self.auth.token_map_path),
        )
        .await?;
        self.filters
            .subscription_map
            .lock()
            .await
            .insert(username.to_owned(), SubscriptionTier::Full);
        Ok(token)
    }

    fn check_join(&self, username: &str) -> Result<(), String> {
        check_join_permission(username, self.config.as_ref())
    }

    async fn host_name(&self) -> Option<String> {
        self.host_user.lock().await.clone()
    }

    async fn status_count(&self) -> usize {
        self.status_map.lock().await.len()
    }

    async fn load_history(&self) -> Vec<Message> {
        crate::history::load(&self.chat_path)
            .await
            .unwrap_or_default()
    }

    async fn route_and_dispatch(
        &self,
        msg: Message,
        username: &str,
    ) -> anyhow::Result<DispatchResult> {
        match route_command(msg, username, self).await? {
            CommandResult::Handled => Ok(DispatchResult::Handled),
            CommandResult::HandledWithReply(json) | CommandResult::Reply(json) => {
                Ok(DispatchResult::Reply(json))
            }
            CommandResult::Shutdown => Ok(DispatchResult::Shutdown),
            CommandResult::Passthrough(msg) => {
                if let Err(reason) = check_send_permission(username, self.config.as_ref()) {
                    return Ok(DispatchResult::SendDenied(reason));
                }
                let seq_msg = match &msg {
                    Message::DirectMessage { .. } => {
                        dm_and_persist(
                            &msg,
                            &self.host_user,
                            &self.clients,
                            &self.chat_path,
                            &self.seq_counter,
                        )
                        .await?
                    }
                    _ => {
                        broadcast_and_persist(
                            &msg,
                            &self.clients,
                            &self.chat_path,
                            &self.seq_counter,
                        )
                        .await?
                    }
                };
                Ok(DispatchResult::Sent(seq_msg))
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::state::RoomState;
    use std::{collections::HashMap, sync::Arc};
    use tempfile::NamedTempFile;
    use tokio::sync::Mutex;

    fn make_state(chat_path: std::path::PathBuf) -> Arc<RoomState> {
        let token_map_path = chat_path.with_extension("tokens");
        let subscription_map_path = chat_path.with_extension("subscriptions");
        RoomState::new(
            "test-room".to_owned(),
            chat_path,
            token_map_path,
            subscription_map_path,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            None,
        )
        .unwrap()
    }

    #[test]
    fn room_id_returns_correct_id() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        assert_eq!(state.room_id(), "test-room");
    }

    #[test]
    fn check_join_public_room_allows_anyone() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        assert!(state.check_join("alice").is_ok());
    }

    #[tokio::test]
    async fn issue_and_validate_token_round_trip() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let token = state.issue_token("alice").await.unwrap();
        assert!(!token.is_empty());
        let username = state.validate_token(&token).await;
        assert_eq!(username.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn validate_token_unknown_returns_none() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        assert!(state.validate_token("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn issue_token_duplicate_username_fails() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.issue_token("alice").await.unwrap();
        assert!(state.issue_token("alice").await.is_err());
    }

    #[tokio::test]
    async fn issue_token_sets_full_subscription() {
        use room_protocol::SubscriptionTier;
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.issue_token("alice").await.unwrap();
        let tier = state
            .filters
            .subscription_map
            .lock()
            .await
            .get("alice")
            .copied();
        assert_eq!(
            tier,
            Some(SubscriptionTier::Full),
            "REST join must set Full so subscribe_mentioned cannot downgrade"
        );
    }

    #[tokio::test]
    async fn host_name_initially_none() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        assert!(state.host_name().await.is_none());
    }

    #[tokio::test]
    async fn status_count_initially_zero() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        assert_eq!(RoomService::status_count(&*state).await, 0);
    }

    #[tokio::test]
    async fn load_history_empty_file() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        assert!(state.load_history().await.is_empty());
    }

    #[tokio::test]
    async fn route_and_dispatch_regular_message_sends() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = crate::message::make_message("test-room", "alice", "hello");
        let result = state.route_and_dispatch(msg, "alice").await.unwrap();
        assert!(matches!(result, DispatchResult::Sent(_)));
    }

    #[tokio::test]
    async fn route_and_dispatch_command_returns_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = crate::message::make_command("test-room", "alice", "who", vec![]);
        let result = state.route_and_dispatch(msg, "alice").await.unwrap();
        assert!(matches!(result, DispatchResult::Reply(_)));
    }

    #[tokio::test]
    async fn route_and_dispatch_dm_in_public_room_sends() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = crate::message::make_dm("test-room", "alice", "bob", "secret");
        let result = state.route_and_dispatch(msg, "alice").await.unwrap();
        assert!(matches!(result, DispatchResult::Sent(_)));
    }

    #[tokio::test]
    async fn route_and_dispatch_dm_in_dm_room_non_participant_denied() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let token_map_path = tmp.path().with_extension("tokens");
        let sub_map_path = tmp.path().with_extension("subscriptions");
        let state = RoomState::new(
            "dm-room".to_owned(),
            tmp.path().to_path_buf(),
            token_map_path,
            sub_map_path,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Some(config),
        )
        .unwrap();
        let msg = crate::message::make_message("dm-room", "eve", "hello");
        let result = state.route_and_dispatch(msg, "eve").await.unwrap();
        assert!(matches!(result, DispatchResult::SendDenied(_)));
    }
}
