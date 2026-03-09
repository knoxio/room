//! Admin command handlers extracted from `commands.rs`.
//!
//! These commands are restricted to the room host and perform privileged
//! operations: kick, reauth, clear-tokens, exit, clear.

use crate::message::make_system;

use super::{fanout::broadcast_and_persist, state::RoomState};

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
            let target = arg.to_owned();
            let mut map = token_map.lock().await;
            // Remove all existing tokens for this username, then insert a per-user sentinel
            // so the username stays reserved. Using KICKED:<username> as the key ensures
            // kicking multiple users does not overwrite each other's sentinel entries.
            map.retain(|_, u| u != &target);
            map.insert(format!("KICKED:{target}"), target.clone());
            drop(map);
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
            let target = arg.to_owned();
            let mut map = token_map.lock().await;
            map.retain(|_, u| u != &target);
            drop(map);
            // Remove the on-disk token file so the user can join afresh.
            let prefix = format!("room-{room_id}-");
            let suffix = format!("-{target}.token");
            if let Ok(entries) = std::fs::read_dir("/tmp") {
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
            token_map.lock().await.clear();
            // Remove all on-disk token files for this room.
            let prefix = format!("room-{room_id}-");
            if let Ok(entries) = std::fs::read_dir("/tmp") {
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
}
