//! Admin command handlers extracted from `commands.rs`.
//!
//! These commands are restricted to the room host and perform privileged
//! operations: kick, reauth, clear-tokens, exit, clear.

use crate::message::make_system;

use super::{auth::save_token_map, fanout::broadcast_and_persist, state::RoomState};

/// Admin command names — routed through `handle_admin_cmd` when received as
/// a `Message::Command` with one of these cmd values.
pub(crate) const ADMIN_CMD_NAMES: &[&str] = &["kick", "reauth", "clear-tokens", "exit", "clear"];

/// Execute an admin command issued by the room host.
///
/// Returns `Some(error_message)` on permission denial or `None` on success.
/// Side-effects: kicks users, clears tokens, truncates history, or shuts down
/// the broker — depending on the command.
///
/// Supported commands:
/// - `/kick <username>`       — invalidates the user's token and inserts a kicked sentinel.
/// - `/reauth <username>`     — removes the user's token so they can rejoin.
/// - `/clear-tokens`          — removes every token for this room (all users must rejoin).
/// - `/exit`                  — broadcasts a shutdown notice then signals the broker to stop.
/// - `/clear`                 — truncates the chat history file and broadcasts a notice.
pub(crate) async fn handle_admin_cmd(
    cmd_line: &str,
    issuer: &str,
    state: &RoomState,
) -> Option<String> {
    // Auth: only the room host may run admin commands.
    let host = state.host_user.lock().await.clone();
    if host.as_deref() != Some(issuer) {
        return Some(
            "permission denied: admin commands are restricted to the room host".to_string(),
        );
    }

    let room_id = state.room_id.as_str();
    let clients = &state.clients;
    let token_map = &state.token_map;
    let chat_path = &state.chat_path;
    let shutdown = &state.shutdown;
    let seq_counter = &state.seq_counter;
    let mut parts = cmd_line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("").trim();
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
        "kick" => {
            if arg.is_empty() {
                return None;
            }
            let target = arg.strip_prefix('@').unwrap_or(arg).to_owned();
            let mut map = token_map.lock().await;
            // Remove all existing tokens for this username, then insert a per-user sentinel
            // so the username stays reserved. Using KICKED:<username> as the key ensures
            // kicking multiple users does not overwrite each other's sentinel entries.
            map.retain(|_, u| u != &target);
            map.insert(format!("KICKED:{target}"), target.clone());
            if let Err(e) = save_token_map(&map, &state.token_map_path) {
                eprintln!("[admin] kick token persist failed: {e}");
            }
            drop(map);
            // In daemon mode, also revoke from the UserRegistry so the kicked user
            // cannot rejoin via issue_token_via_registry (which checks has_token_for_user).
            if let Some(reg) = state.registry.get() {
                if let Err(e) = reg.lock().await.revoke_user_tokens(&target) {
                    eprintln!("[admin] registry revoke failed for {target}: {e}");
                }
            }
            // Remove from status map immediately so /who no longer shows the kicked user.
            state.remove_status(&target).await;
            let content = format!("{issuer} kicked {target} (token invalidated)");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        "reauth" => {
            if arg.is_empty() {
                return None;
            }
            let target = arg.strip_prefix('@').unwrap_or(arg).to_owned();
            let mut map = token_map.lock().await;
            map.retain(|_, u| u != &target);
            if let Err(e) = save_token_map(&map, &state.token_map_path) {
                eprintln!("[admin] reauth token persist failed: {e}");
            }
            drop(map);
            // In daemon mode, also revoke from the UserRegistry so the reauthed user
            // can obtain a fresh token via issue_token_via_registry.
            if let Some(reg) = state.registry.get() {
                if let Err(e) = reg.lock().await.revoke_user_tokens(&target) {
                    eprintln!("[admin] registry revoke failed for {target}: {e}");
                }
            }
            // Remove the on-disk token file so the user can join afresh.
            let state_dir = crate::paths::room_state_dir();
            let prefix = format!("room-{room_id}-");
            let suffix = format!("-{target}.token");
            if let Ok(entries) = std::fs::read_dir(&state_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let s = name.to_string_lossy();
                    if s.starts_with(&prefix) && s.ends_with(&suffix) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            let content = format!("{issuer} reauthed {target} (token cleared, can rejoin)");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        "clear-tokens" => {
            let mut map = token_map.lock().await;
            map.clear();
            if let Err(e) = save_token_map(&map, &state.token_map_path) {
                eprintln!("[admin] clear-tokens persist failed: {e}");
            }
            drop(map);
            // Remove all on-disk token files for this room.
            let state_dir = crate::paths::room_state_dir();
            let prefix = format!("room-{room_id}-");
            if let Ok(entries) = std::fs::read_dir(&state_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let s = name.to_string_lossy();
                    if s.starts_with(&prefix) && s.ends_with(".token") {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
            let content = format!("{issuer} cleared all tokens (all users must rejoin)");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        "exit" => {
            let content = format!("{issuer} is shutting down the room");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
            // Set to true — watch receivers see this immediately regardless of
            // when they registered, avoiding the notify_waiters() race.
            let _ = shutdown.send(true);
        }
        "clear" => {
            // Truncate the history file.
            if let Err(e) = std::fs::write(chat_path.as_ref(), "") {
                eprintln!("[broker] \\clear failed: {e}");
                return None;
            }
            let content = format!("{issuer} cleared chat history");
            let msg = make_system(room_id, "broker", content);
            let _ = broadcast_and_persist(&msg, clients, chat_path, seq_counter).await;
        }
        _ => {
            eprintln!("[broker] unknown admin command from {issuer}: \\{cmd_line}");
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::handle_admin_cmd;
    use crate::{broker::state::RoomState, registry::UserRegistry};
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

    #[tokio::test]
    async fn handle_admin_cmd_reauth_removes_token_and_sentinel() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        {
            let mut guard = state.token_map.lock().await;
            guard.insert("uuid-bob".to_owned(), "bob".to_owned());
            guard.insert("KICKED:bob".to_owned(), "bob".to_owned());
        }
        let err = handle_admin_cmd("reauth bob", "alice", &state).await;
        assert!(err.is_none(), "reauth should succeed");
        let guard = state.token_map.lock().await;
        assert!(
            !guard.values().any(|u| u == "bob"),
            "all bob entries must be removed after reauth"
        );
    }

    #[tokio::test]
    async fn handle_admin_cmd_clear_tokens_empties_the_map() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        {
            let mut guard = state.token_map.lock().await;
            guard.insert("t1".to_owned(), "alice".to_owned());
            guard.insert("t2".to_owned(), "bob".to_owned());
        }
        let err = handle_admin_cmd("clear-tokens", "alice", &state).await;
        assert!(err.is_none(), "clear-tokens should succeed");
        assert!(
            state.token_map.lock().await.is_empty(),
            "token map must be empty after clear-tokens"
        );
    }

    #[tokio::test]
    async fn handle_admin_cmd_clear_removes_prior_history() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"some existing history\n").unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let err = handle_admin_cmd("clear", "alice", &state).await;
        assert!(err.is_none(), "clear should succeed");
        // The file is truncated first then a "cleared" notice is appended;
        // old content must not be present.
        let contents = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            !contents.contains("some existing history"),
            "prior history must be gone after clear"
        );
    }

    #[tokio::test]
    async fn handle_admin_cmd_non_host_gets_permission_denied() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let err = handle_admin_cmd("exit", "bob", &state).await;
        assert!(err.is_some(), "non-host should be denied");
        assert!(
            err.unwrap().contains("permission denied"),
            "error should mention permission denied"
        );
    }

    #[tokio::test]
    async fn handle_admin_cmd_kick_invalidates_token() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        state
            .token_map
            .lock()
            .await
            .insert("tok-bob".to_owned(), "bob".to_owned());
        let err = handle_admin_cmd("kick bob", "alice", &state).await;
        assert!(err.is_none(), "kick should succeed");
        let map = state.token_map.lock().await;
        assert!(
            !map.contains_key("tok-bob"),
            "bob's token should be removed"
        );
        assert!(
            map.get("KICKED:bob").is_some(),
            "kicked sentinel should exist"
        );
    }

    #[tokio::test]
    async fn handle_admin_cmd_exit_sends_shutdown() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        // Hold a receiver so send() does not silently fail.
        let mut rx = state.shutdown.subscribe();
        let err = handle_admin_cmd("exit", "alice", &state).await;
        assert!(err.is_none(), "exit should succeed");
        // The receiver should see the updated value.
        assert!(
            *rx.borrow_and_update(),
            "shutdown signal should be true after /exit"
        );
    }

    // ── registry-aware admin commands ─────────────────────────────────────────

    fn make_state_with_registry(
        chat_path: std::path::PathBuf,
        registry: Arc<Mutex<UserRegistry>>,
    ) -> Arc<RoomState> {
        let token_map_path = chat_path.with_extension("tokens");
        let subscription_map_path = chat_path.with_extension("subscriptions");
        let state = RoomState::new(
            "test-room".to_owned(),
            chat_path,
            token_map_path,
            subscription_map_path,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            None,
        )
        .unwrap();
        state.set_registry(registry);
        state
    }

    #[tokio::test]
    async fn kick_revokes_token_from_registry_in_daemon_mode() {
        let tmp = NamedTempFile::new().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Mutex::new(UserRegistry::new(reg_dir.path().to_owned())));

        // Register bob and issue a token in the registry.
        let bob_token = {
            let mut reg = registry.lock().await;
            reg.register_user("bob").unwrap();
            reg.issue_token("bob").unwrap()
        };

        let state = make_state_with_registry(tmp.path().to_path_buf(), registry.clone());
        *state.host_user.lock().await = Some("alice".to_owned());
        state
            .token_map
            .lock()
            .await
            .insert(bob_token.clone(), "bob".to_owned());

        let err = handle_admin_cmd("kick bob", "alice", &state).await;
        assert!(err.is_none(), "kick should succeed");

        // Registry token must be gone so bob cannot rejoin via issue_token_via_registry.
        let reg = registry.lock().await;
        assert!(
            reg.validate_token(&bob_token).is_none(),
            "kick must revoke bob's token from the registry"
        );
        assert!(
            !reg.has_token_for_user("bob"),
            "kick must remove all registry tokens for bob"
        );
    }

    // ── token persistence after admin commands (#464) ──────────────────────

    #[tokio::test]
    async fn kick_persists_token_map_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let chat_path = dir.path().join("test.chat");
        std::fs::write(&chat_path, "").unwrap();
        let state = make_state(chat_path);
        *state.host_user.lock().await = Some("alice".to_owned());
        state
            .token_map
            .lock()
            .await
            .insert("tok-bob".to_owned(), "bob".to_owned());

        handle_admin_cmd("kick bob", "alice", &state).await;

        // Load from disk — KICKED sentinel must be persisted
        let loaded = crate::broker::auth::load_token_map(&state.token_map_path);
        assert!(
            loaded.contains_key("KICKED:bob"),
            "KICKED sentinel must be persisted to disk"
        );
        assert!(
            !loaded.contains_key("tok-bob"),
            "original token must not be on disk after kick"
        );
    }

    #[tokio::test]
    async fn reauth_persists_token_map_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let chat_path = dir.path().join("test.chat");
        std::fs::write(&chat_path, "").unwrap();
        let state = make_state(chat_path);
        *state.host_user.lock().await = Some("alice".to_owned());
        {
            let mut map = state.token_map.lock().await;
            map.insert("tok-bob".to_owned(), "bob".to_owned());
            map.insert("KICKED:bob".to_owned(), "bob".to_owned());
        }

        handle_admin_cmd("reauth bob", "alice", &state).await;

        let loaded = crate::broker::auth::load_token_map(&state.token_map_path);
        assert!(
            !loaded.values().any(|u| u == "bob"),
            "bob must be fully removed from disk after reauth"
        );
    }

    #[tokio::test]
    async fn clear_tokens_persists_empty_map_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let chat_path = dir.path().join("test.chat");
        std::fs::write(&chat_path, "").unwrap();
        let state = make_state(chat_path);
        *state.host_user.lock().await = Some("alice".to_owned());
        {
            let mut map = state.token_map.lock().await;
            map.insert("t1".to_owned(), "alice".to_owned());
            map.insert("t2".to_owned(), "bob".to_owned());
        }

        handle_admin_cmd("clear-tokens", "alice", &state).await;

        let loaded = crate::broker::auth::load_token_map(&state.token_map_path);
        assert!(
            loaded.is_empty(),
            "token map on disk must be empty after clear-tokens"
        );
    }

    #[tokio::test]
    async fn kicked_token_rejected_after_simulated_restart() {
        let dir = tempfile::tempdir().unwrap();
        let chat_path = dir.path().join("test.chat");
        std::fs::write(&chat_path, "").unwrap();
        let state = make_state(chat_path.clone());
        *state.host_user.lock().await = Some("alice".to_owned());
        state
            .token_map
            .lock()
            .await
            .insert("tok-bob".to_owned(), "bob".to_owned());

        handle_admin_cmd("kick bob", "alice", &state).await;

        // Simulate broker restart: load token map from disk into fresh state
        let loaded = crate::broker::auth::load_token_map(&state.token_map_path);
        let new_map: super::super::state::TokenMap = Arc::new(Mutex::new(loaded));

        // The original token must be invalid
        assert!(
            crate::broker::auth::validate_token("tok-bob", &new_map)
                .await
                .is_none(),
            "original token must be invalid after restart"
        );
        // The KICKED sentinel must not authenticate
        assert!(
            crate::broker::auth::validate_token("KICKED:bob", &new_map)
                .await
                .is_none(),
            "KICKED sentinel must not authenticate after restart"
        );
    }

    // ── @-prefix stripping (#505) ──────────────────────────────────────────

    #[tokio::test]
    async fn kick_at_prefix_strips_and_invalidates() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        state
            .token_map
            .lock()
            .await
            .insert("tok-bob".to_owned(), "bob".to_owned());
        // User types `/kick @bob` — the @ must be stripped.
        let err = handle_admin_cmd("kick @bob", "alice", &state).await;
        assert!(err.is_none(), "kick @bob should succeed");
        let map = state.token_map.lock().await;
        assert!(
            !map.contains_key("tok-bob"),
            "bob's token should be removed even with @ prefix"
        );
        assert!(
            map.get("KICKED:bob").is_some(),
            "sentinel must use bare username, not @bob"
        );
    }

    #[tokio::test]
    async fn reauth_at_prefix_strips_and_clears() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        {
            let mut guard = state.token_map.lock().await;
            guard.insert("uuid-bob".to_owned(), "bob".to_owned());
            guard.insert("KICKED:bob".to_owned(), "bob".to_owned());
        }
        // User types `/reauth @bob` — the @ must be stripped.
        let err = handle_admin_cmd("reauth @bob", "alice", &state).await;
        assert!(err.is_none(), "reauth @bob should succeed");
        let guard = state.token_map.lock().await;
        assert!(
            !guard.values().any(|u| u == "bob"),
            "all bob entries must be removed even with @ prefix"
        );
    }

    #[tokio::test]
    async fn kick_without_at_prefix_still_works() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        state
            .token_map
            .lock()
            .await
            .insert("tok-carol".to_owned(), "carol".to_owned());
        let err = handle_admin_cmd("kick carol", "alice", &state).await;
        assert!(err.is_none(), "kick carol should succeed");
        assert!(
            state.token_map.lock().await.get("KICKED:carol").is_some(),
            "kick without @ must still work"
        );
    }

    // ── registry-aware admin commands ─────────────────────────────────────────

    #[tokio::test]
    async fn reauth_revokes_token_from_registry_in_daemon_mode() {
        let tmp = NamedTempFile::new().unwrap();
        let reg_dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Mutex::new(UserRegistry::new(reg_dir.path().to_owned())));

        // Register bob and issue a token in the registry.
        let bob_token = {
            let mut reg = registry.lock().await;
            reg.register_user("bob").unwrap();
            reg.issue_token("bob").unwrap()
        };

        let state = make_state_with_registry(tmp.path().to_path_buf(), registry.clone());
        *state.host_user.lock().await = Some("alice".to_owned());
        state
            .token_map
            .lock()
            .await
            .insert(bob_token.clone(), "bob".to_owned());

        let err = handle_admin_cmd("reauth bob", "alice", &state).await;
        assert!(err.is_none(), "reauth should succeed");

        // Registry token must be gone so bob can obtain a new token on the next join.
        let reg = registry.lock().await;
        assert!(
            reg.validate_token(&bob_token).is_none(),
            "reauth must revoke bob's token from the registry"
        );
        assert!(
            !reg.has_token_for_user("bob"),
            "reauth must remove all registry tokens for bob so rejoin is unblocked"
        );
    }
}
