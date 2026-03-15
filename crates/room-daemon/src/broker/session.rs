//! Shared session lifecycle logic for UDS and WebSocket transports.
//!
//! Both transports follow the same lifecycle:
//! 1. **Setup** — register client, elect host, replay history, broadcast join
//! 2. **Inbound** — parse each message, route commands, broadcast/persist
//! 3. **Teardown** — remove status, broadcast leave
//!
//! Transport-specific code (reading frames/lines, writing bytes/WS messages)
//! stays in `broker/mod.rs` (UDS) and `broker/ws/mod.rs` (WS). This module
//! contains only the transport-independent business logic.

use std::sync::Arc;

use crate::history;
use room_protocol::SubscriptionTier;
use room_protocol::{make_join, make_leave, make_system, parse_client_line, Message};

use super::{
    auth,
    commands::{route_command, CommandResult},
    fanout::{broadcast_and_persist, dm_and_persist},
    persistence,
    state::RoomState,
};

// ── Result types ─────────────────────────────────────────────────────────────

/// Result of processing a single inbound message line.
pub(crate) enum InboundResult {
    /// Nothing to send back to the client.
    Ok,
    /// Send this JSON string back to the client.
    Reply(String),
    /// The session should terminate (e.g. `/exit`).
    Shutdown,
}

/// Result of processing a one-shot send request.
pub(crate) enum OneshotResult {
    /// Send this string back to the caller and close.
    Reply(String),
}

// ── Session setup ────────────────────────────────────────────────────────────

/// Set up an interactive session: register client, elect host, replay history,
/// broadcast join event.
///
/// Returns the history lines (already DM-filtered) that the transport should
/// send to the client. Each line is a JSON string WITHOUT a trailing newline —
/// the transport adds its own framing (UDS appends `\n`, WS sends as a text
/// frame).
pub(crate) async fn session_setup(
    cid: u64,
    username: &str,
    state: &Arc<RoomState>,
) -> anyhow::Result<Vec<String>> {
    let username = username.to_owned();

    // Register username in the client map.
    {
        let mut map = state.clients.lock().await;
        if let Some(entry) = map.get_mut(&cid) {
            entry.0 = username.clone();
        }
    }

    // Register as host if no host has been set yet (first to complete handshake).
    // Persist the host username to the room meta file so oneshot commands (poll,
    // pull, query) can apply the same DM visibility rules without a live broker.
    {
        let mut host = state.host_user.lock().await;
        if host.is_none() {
            *host = Some(username.clone());
            let meta_path = crate::paths::room_meta_path(&state.room_id);
            if meta_path.exists() {
                if let Ok(data) = std::fs::read_to_string(&meta_path) {
                    if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&data) {
                        v["host"] = serde_json::Value::String(username.clone());
                        let _ = std::fs::write(&meta_path, v.to_string());
                    }
                }
            }
        }
    }

    eprintln!("[broker] {username} joined (cid={cid})");

    // Track this user in the status map (empty status by default).
    state
        .status_map
        .lock()
        .await
        .insert(username.clone(), String::new());

    // Load chat history, filtering DMs the client is not party to.
    let host_name = state.host_user.lock().await.clone();
    let history = history::load(&state.chat_path).await.unwrap_or_default();
    let mut lines = Vec::with_capacity(history.len());
    for msg in &history {
        if msg.is_visible_to(&username, host_name.as_deref()) {
            lines.push(serde_json::to_string(msg)?);
        }
    }

    // Broadcast join event (also persists it).
    let join_msg = make_join(&state.room_id, &username);
    broadcast_and_persist(
        &join_msg,
        &state.clients,
        &state.chat_path,
        &state.seq_counter,
    )
    .await
    .map_err(|e| anyhow::anyhow!("broadcast_and_persist(join) failed: {e:#}"))?;
    state.plugin_registry.notify_join(&username);

    Ok(lines)
}

// ── Session teardown ─────────────────────────────────────────────────────────

/// Clean up after an interactive session: remove status, broadcast leave event.
pub(crate) async fn session_teardown(cid: u64, username: &str, state: &Arc<RoomState>) {
    // Remove user from status map on disconnect.
    state.status_map.lock().await.remove(username);

    // Broadcast leave event.
    let leave_msg = make_leave(&state.room_id, username);
    let _ = broadcast_and_persist(
        &leave_msg,
        &state.clients,
        &state.chat_path,
        &state.seq_counter,
    )
    .await;
    state.plugin_registry.notify_leave(username);
    eprintln!("[broker] {username} left (cid={cid})");
}

// ── Inbound message processing ───────────────────────────────────────────────

/// Process a single inbound message line from an interactive client.
///
/// Handles command routing, DM permission checks, @mention auto-subscription,
/// and broadcast/persist. The transport only needs to deliver the reply (if any)
/// back to the client.
pub(crate) async fn process_inbound_message(
    trimmed: &str,
    username: &str,
    state: &Arc<RoomState>,
) -> InboundResult {
    let msg = match parse_client_line(trimmed, &state.room_id, username) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[broker] bad message from {username}: {e}");
            return InboundResult::Ok;
        }
    };

    match route_command(msg, username, state).await {
        Ok(CommandResult::Handled | CommandResult::HandledWithReply(_)) => InboundResult::Ok,
        Ok(CommandResult::Reply(json)) => InboundResult::Reply(json),
        Ok(CommandResult::Shutdown) => InboundResult::Shutdown,
        Ok(CommandResult::Passthrough(msg)) => process_passthrough(&msg, username, state).await,
        Err(e) => {
            eprintln!("[broker] route error: {e:#}");
            InboundResult::Ok
        }
    }
}

/// Handle a passthrough message: check send permission, auto-subscribe mentions,
/// then broadcast or DM.
async fn process_passthrough(msg: &Message, username: &str, state: &RoomState) -> InboundResult {
    // DM privacy: reject sends from non-participants.
    if let Err(reason) = auth::check_send_permission(username, state.config.as_ref()) {
        let err = serde_json::json!({
            "type": "error",
            "code": "send_denied",
            "message": reason
        });
        return InboundResult::Reply(format!("{err}"));
    }

    let is_broadcast = !matches!(msg, Message::DirectMessage { .. });

    // Subscribe @mentioned users BEFORE broadcast so the
    // subscription is on disk before the message (#481).
    let newly_subscribed = if is_broadcast {
        subscribe_mentioned(msg, state).await
    } else {
        Vec::new()
    };

    let result = match msg {
        Message::DirectMessage { .. } => {
            dm_and_persist(
                msg,
                &state.host_user,
                &state.clients,
                &state.chat_path,
                &state.seq_counter,
            )
            .await
        }
        _ => broadcast_and_persist(msg, &state.clients, &state.chat_path, &state.seq_counter).await,
    };

    if let Err(e) = &result {
        eprintln!("[broker] persist error: {e:#}");
    }

    // Notify plugins about the broadcast message so they can track activity.
    if let Ok(ref persisted) = result {
        state.plugin_registry.notify_message(persisted);
    }

    if !newly_subscribed.is_empty() && result.is_ok() {
        broadcast_subscribe_notices(&newly_subscribed, state).await;
    }

    InboundResult::Ok
}

// ── Oneshot send processing ──────────────────────────────────────────────────

/// Process a one-shot send: parse the message, route it, and return the
/// response string that the transport should send back to the caller.
pub(crate) async fn process_oneshot_send(
    trimmed: &str,
    username: &str,
    state: &RoomState,
) -> anyhow::Result<OneshotResult> {
    let msg = match parse_client_line(trimmed, &state.room_id, username) {
        Ok(m) => m,
        Err(e) => {
            let err = serde_json::json!({
                "type": "error",
                "code": "parse_error",
                "message": format!("{e:#}")
            });
            return Ok(OneshotResult::Reply(format!("{err}")));
        }
    };

    let cmd_result = match route_command(msg, username, state).await {
        Ok(r) => r,
        Err(e) => {
            let err = serde_json::json!({
                "type": "error",
                "code": "route_error",
                "message": format!("{e:#}")
            });
            return Ok(OneshotResult::Reply(format!("{err}")));
        }
    };

    match cmd_result {
        CommandResult::Handled | CommandResult::Shutdown => {
            let ack = make_system(&state.room_id, "broker", "ok");
            let json = serde_json::to_string(&ack)?;
            Ok(OneshotResult::Reply(json))
        }
        CommandResult::HandledWithReply(json) | CommandResult::Reply(json) => {
            Ok(OneshotResult::Reply(json))
        }
        CommandResult::Passthrough(msg) => {
            // DM privacy: reject sends from non-participants.
            if let Err(reason) = auth::check_send_permission(username, state.config.as_ref()) {
                let err = serde_json::json!({
                    "type": "error",
                    "code": "send_denied",
                    "message": reason
                });
                return Ok(OneshotResult::Reply(format!("{err}")));
            }

            let is_broadcast = !matches!(&msg, Message::DirectMessage { .. });

            // Subscribe @mentioned users BEFORE broadcast (#481).
            let newly_subscribed = if is_broadcast {
                subscribe_mentioned(&msg, state).await
            } else {
                Vec::new()
            };

            let seq_msg = match &msg {
                Message::DirectMessage { .. } => {
                    dm_and_persist(
                        &msg,
                        &state.host_user,
                        &state.clients,
                        &state.chat_path,
                        &state.seq_counter,
                    )
                    .await?
                }
                _ => {
                    broadcast_and_persist(
                        &msg,
                        &state.clients,
                        &state.chat_path,
                        &state.seq_counter,
                    )
                    .await?
                }
            };

            if !newly_subscribed.is_empty() {
                broadcast_subscribe_notices(&newly_subscribed, state).await;
            }

            let echo = serde_json::to_string(&seq_msg)?;
            Ok(OneshotResult::Reply(echo))
        }
    }
}

// ── Mention auto-subscription ────────────────────────────────────────────────

/// Subscribe @mentioned users who are not already subscribed (or are `Unsubscribed`).
///
/// Must be called BEFORE `broadcast_and_persist` so that the subscription exists
/// on disk before the message is persisted to the chat file. This ensures poll-based
/// room discovery (`discover_joined_rooms`) finds the room before the mention message
/// is written, closing the race window described in #481.
///
/// Returns the list of newly subscribed usernames. Callers should pass this to
/// [`broadcast_subscribe_notices`] after the message has been broadcast.
pub(crate) async fn subscribe_mentioned(msg: &Message, state: &RoomState) -> Vec<String> {
    let mentioned = msg.mentions();
    if mentioned.is_empty() {
        return Vec::new();
    }

    // Collect users to auto-subscribe (brief lock hold).
    let newly_subscribed = {
        let token_map = state.auth.token_map.lock().await;
        let registered: std::collections::HashSet<&str> =
            token_map.values().map(String::as_str).collect();

        let mut sub_map = state.filters.subscription_map.lock().await;
        let mut newly = Vec::new();

        for username in &mentioned {
            if !registered.contains(username.as_str()) {
                continue;
            }
            let dominated = match sub_map.get(username.as_str()) {
                None | Some(SubscriptionTier::Unsubscribed) => true,
                Some(_) => false,
            };
            if dominated {
                sub_map.insert(username.clone(), SubscriptionTier::MentionsOnly);
                newly.push(username.clone());
            }
        }
        newly
    };

    if !newly_subscribed.is_empty() {
        // Persist the updated subscription map to disk so that
        // `discover_joined_rooms` picks up the new room immediately.
        persistence::persist_subscriptions(state).await;
    }

    newly_subscribed
}

/// Broadcast system notices for users that were auto-subscribed by [`subscribe_mentioned`].
///
/// Call this AFTER the original message has been broadcast so that the notice
/// appears after the mention in chat history.
pub(crate) async fn broadcast_subscribe_notices(newly_subscribed: &[String], state: &RoomState) {
    for username in newly_subscribed {
        let notice = format!(
            "{username} auto-subscribed at mentions_only (mentioned in {})",
            state.room_id
        );
        let sys = make_system(&state.room_id, "broker", notice);
        let _ =
            broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await;
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginRegistry;
    use room_protocol::make_message;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;
    use tokio::sync::{watch, Mutex};

    fn make_test_state(chat_path: std::path::PathBuf) -> Arc<RoomState> {
        use crate::broker::state::{AuthState, FilterState};
        let (shutdown_tx, _) = watch::channel(false);
        Arc::new(RoomState {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            auth: AuthState {
                token_map: Arc::new(Mutex::new(HashMap::new())),
                token_map_path: Arc::new(chat_path.with_extension("tokens")),
                registry: std::sync::OnceLock::new(),
            },
            filters: FilterState {
                subscription_map: Arc::new(Mutex::new(HashMap::new())),
                subscription_map_path: Arc::new(chat_path.with_extension("subscriptions")),
                event_filter_state: std::sync::OnceLock::new(),
            },
            chat_path: Arc::new(chat_path.clone()),
            room_id: Arc::new("test-room".to_owned()),
            shutdown: Arc::new(shutdown_tx),
            seq_counter: Arc::new(AtomicU64::new(0)),
            plugin_registry: Arc::new(PluginRegistry::new()),
            config: None,
            cross_room_resolver: std::sync::OnceLock::new(),
        })
    }

    #[tokio::test]
    async fn auto_subscribe_skips_unregistered_users() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        let msg = make_message("test-room", "bob", "hey @alice check this");
        subscribe_mentioned(&msg, &state).await;
        assert!(state.filters.subscription_map.lock().await.is_empty());
    }

    #[tokio::test]
    async fn auto_subscribe_registers_mentions_only_for_unsubscribed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hey @alice check this");
        subscribe_mentioned(&msg, &state).await;
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn auto_subscribe_skips_already_subscribed_full() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        state
            .filters
            .subscription_map
            .lock()
            .await
            .insert("alice".to_owned(), SubscriptionTier::Full);
        let msg = make_message("test-room", "bob", "hey @alice check this");
        subscribe_mentioned(&msg, &state).await;
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::Full
        );
    }

    #[tokio::test]
    async fn auto_subscribe_skips_already_subscribed_mentions_only() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        state
            .filters
            .subscription_map
            .lock()
            .await
            .insert("alice".to_owned(), SubscriptionTier::MentionsOnly);
        let msg = make_message("test-room", "bob", "@alice ping");
        subscribe_mentioned(&msg, &state).await;
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn auto_subscribe_upgrades_unsubscribed_to_mentions_only() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        state
            .filters
            .subscription_map
            .lock()
            .await
            .insert("alice".to_owned(), SubscriptionTier::Unsubscribed);
        let msg = make_message("test-room", "bob", "@alice come back");
        subscribe_mentioned(&msg, &state).await;
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn auto_subscribe_handles_multiple_mentions() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        {
            let mut tokens = state.auth.token_map.lock().await;
            tokens.insert("tok-alice".to_owned(), "alice".to_owned());
            tokens.insert("tok-carol".to_owned(), "carol".to_owned());
        }
        let msg = make_message("test-room", "bob", "@alice @carol @unknown review this");
        subscribe_mentioned(&msg, &state).await;
        let sub_map = state.filters.subscription_map.lock().await;
        assert_eq!(
            *sub_map.get("alice").unwrap(),
            SubscriptionTier::MentionsOnly
        );
        assert_eq!(
            *sub_map.get("carol").unwrap(),
            SubscriptionTier::MentionsOnly
        );
        assert!(sub_map.get("unknown").is_none());
    }

    #[tokio::test]
    async fn auto_subscribe_no_op_for_message_without_mentions() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hello everyone");
        subscribe_mentioned(&msg, &state).await;
        assert!(state.filters.subscription_map.lock().await.is_empty());
    }

    #[tokio::test]
    async fn auto_subscribe_broadcasts_notice() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hey @alice");
        let newly = subscribe_mentioned(&msg, &state).await;
        broadcast_subscribe_notices(&newly, &state).await;
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("auto-subscribed"));
        assert!(history.contains("alice"));
        assert!(history.contains("mentions_only"));
    }

    #[tokio::test]
    async fn auto_subscribe_persists_to_disk() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hey @alice");
        subscribe_mentioned(&msg, &state).await;
        let loaded = persistence::load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::MentionsOnly));
    }

    #[tokio::test]
    async fn subscribe_mentioned_returns_newly_subscribed_before_broadcast() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .auth
            .token_map
            .lock()
            .await
            .insert("tok-alice".to_owned(), "alice".to_owned());
        let msg = make_message("test-room", "bob", "hey @alice check this");

        let newly = subscribe_mentioned(&msg, &state).await;
        assert_eq!(newly, vec!["alice"]);

        let loaded = persistence::load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::MentionsOnly));
        let chat_content = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            chat_content.is_empty(),
            "chat file must be empty before broadcast — subscription should precede message"
        );

        let seq_msg =
            broadcast_and_persist(&msg, &state.clients, &state.chat_path, &state.seq_counter)
                .await
                .unwrap();
        assert!(seq_msg.seq().is_some());

        broadcast_subscribe_notices(&newly, &state).await;

        let history = std::fs::read_to_string(tmp.path()).unwrap();
        let lines: Vec<&str> = history.trim().lines().collect();
        assert_eq!(lines.len(), 2, "expected message + notice");
        assert!(lines[0].contains("hey @alice check this"));
        assert!(lines[1].contains("auto-subscribed"));
    }

    #[tokio::test]
    async fn session_setup_registers_host() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        // Insert client entry first (simulating what handle_client does).
        let (tx, _) = tokio::sync::broadcast::channel::<String>(16);
        state.clients.lock().await.insert(1, (String::new(), tx));

        let lines = session_setup(1, "alice", &state).await.unwrap();
        // No history, so no lines to replay.
        assert!(lines.is_empty());
        // Alice should be host.
        assert_eq!(state.host_user.lock().await.as_deref(), Some("alice"));
        // Alice should be in the status map.
        assert!(state.status_map.lock().await.contains_key("alice"));
    }

    #[tokio::test]
    async fn session_setup_does_not_overwrite_existing_host() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("existing-host".to_owned());

        let (tx, _) = tokio::sync::broadcast::channel::<String>(16);
        state.clients.lock().await.insert(1, (String::new(), tx));

        session_setup(1, "bob", &state).await.unwrap();
        assert_eq!(
            state.host_user.lock().await.as_deref(),
            Some("existing-host")
        );
    }

    #[tokio::test]
    async fn session_teardown_removes_status_and_broadcasts_leave() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        state
            .status_map
            .lock()
            .await
            .insert("alice".to_owned(), "working".to_owned());

        session_teardown(1, "alice", &state).await;

        assert!(!state.status_map.lock().await.contains_key("alice"));
        // Leave event should be persisted.
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("\"type\":\"leave\""));
        assert!(history.contains("alice"));
    }

    #[tokio::test]
    async fn process_inbound_message_routes_plain_text() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        let result = process_inbound_message("hello world", "bob", &state).await;
        // Plain text is a Passthrough → broadcast → no reply to sender.
        assert!(matches!(result, InboundResult::Ok));
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("hello world"));
    }

    #[tokio::test]
    async fn process_oneshot_send_returns_echo() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        let result = process_oneshot_send("hello oneshot", "bob", &state)
            .await
            .unwrap();
        let OneshotResult::Reply(json) = result;
        assert!(json.contains("hello oneshot"));
    }

    #[tokio::test]
    async fn process_oneshot_send_rejects_bad_parse() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let state = make_test_state(tmp.path().to_path_buf());
        // An invalid JSON envelope should return a parse_error.
        let result = process_oneshot_send("{invalid json", "bob", &state)
            .await
            .unwrap();
        let OneshotResult::Reply(json) = result;
        assert!(json.contains("parse_error") || json.contains("route_error"));
    }
}
