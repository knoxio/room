use crate::{
    message::{make_system, Message},
    plugin::{
        builtin_command_infos, ChatWriter, CommandContext, CommandInfo, HistoryReader, ParamType,
        PluginResult, RoomMetadata,
    },
};

use super::{fanout::broadcast_and_persist, state::RoomState};

/// Admin command names — routed through `handle_admin_cmd` when received as
/// a `Message::Command` with one of these cmd values.
pub(crate) const ADMIN_CMD_NAMES: &[&str] = &["kick", "reauth", "clear-tokens", "exit", "clear"];

/// The result of routing an inbound command line.
pub(crate) enum CommandResult {
    /// The command was fully handled with a broadcast or no-op; nothing to send back privately.
    Handled,
    /// The command was handled with a broadcast; oneshot callers should receive this JSON echo.
    ///
    /// Interactive clients already receive the message via the broadcast channel, so
    /// `handle_client` treats this identically to `Handled`. One-shot senders are not
    /// subscribed to the broadcast, so `handle_oneshot_send` writes the JSON back to them
    /// directly, avoiding the EOF parse error the client would otherwise see.
    HandledWithReply(String),
    /// The command was handled privately; send this JSON line back only to the issuer.
    Reply(String),
    /// The command was handled and the broker is shutting down.
    Shutdown,
    /// Not a special command; the caller should broadcast or DM `msg` normally.
    Passthrough(Message),
}

/// Route a parsed `Message` for a given `username` against `state`.
///
/// Handles `set_status`, `who`, and all admin commands inline. For any other
/// message (including regular chat) returns `CommandResult::Passthrough(msg)`
/// so the caller can broadcast or DM it.
///
/// This is the unified entry point used by both the interactive inbound task
/// and `handle_oneshot_send` — previously the logic was duplicated in both.
pub(crate) async fn route_command(
    msg: Message,
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    if let Message::Command {
        ref cmd,
        ref params,
        ..
    } = msg
    {
        // Validate params against built-in schema before dispatching.
        let builtins = builtin_command_infos();
        if let Some(cmd_info) = builtins.iter().find(|c| c.name == *cmd) {
            if let Err(err_msg) = validate_params(params, cmd_info) {
                let sys = make_system(&state.room_id, "broker", err_msg);
                let json = serde_json::to_string(&sys)?;
                return Ok(CommandResult::Reply(json));
            }
        }

        if cmd == "set_status" {
            let status = params.first().cloned().unwrap_or_default();
            state
                .status_map
                .lock()
                .await
                .insert(username.to_owned(), status.clone());
            let display = if status.is_empty() {
                format!("{username} cleared their status")
            } else {
                format!("{username} set status: {status}")
            };
            let sys = make_system(&state.room_id, "broker", display);
            broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter)
                .await?;
            // Broadcast already delivers to all interactive clients. One-shot callers are not
            // subscribed to the broadcast channel, so we carry the JSON in HandledWithReply so
            // handle_oneshot_send can write it back — preventing the EOF parse error.
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::HandledWithReply(json));
        }

        if cmd == "who" {
            let map = state.status_map.lock().await;
            let mut entries: Vec<String> = map
                .iter()
                .map(|(u, s)| {
                    if s.is_empty() {
                        u.clone()
                    } else {
                        format!("{u}: {s}")
                    }
                })
                .collect();
            entries.sort();
            drop(map);
            let content = if entries.is_empty() {
                "no users online".to_owned()
            } else {
                format!("online — {}", entries.join(", "))
            };
            let sys = make_system(&state.room_id, "broker", content);
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }

        // Task claim commands.
        if cmd == "claim" {
            let task = params.join(" ");
            state
                .claim_map
                .lock()
                .await
                .insert(username.to_owned(), task.clone());
            let display = format!("{username} claimed: {task}");
            let sys = make_system(&state.room_id, "broker", display);
            broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter)
                .await?;
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::HandledWithReply(json));
        }

        if cmd == "unclaim" {
            let removed = state.claim_map.lock().await.remove(username);
            let display = match removed {
                Some(task) => format!("{username} released claim: {task}"),
                None => format!("{username} has no active claim"),
            };
            let sys = make_system(&state.room_id, "broker", display);
            broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter)
                .await?;
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::HandledWithReply(json));
        }

        if cmd == "claimed" {
            let map = state.claim_map.lock().await;
            let content = if map.is_empty() {
                "no active claims".to_owned()
            } else {
                let mut entries: Vec<String> = map
                    .iter()
                    .map(|(user, task)| format!("{user}: {task}"))
                    .collect();
                entries.sort();
                format!("claimed — {}", entries.join(", "))
            };
            drop(map);
            let sys = make_system(&state.room_id, "broker", content);
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }

        // Room management commands.
        if cmd == "room-info" {
            let result = handle_room_info(state).await;
            let sys = make_system(&state.room_id, "broker", result);
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }

        if ADMIN_CMD_NAMES.contains(&cmd.as_str()) {
            let cmd_line = format!("{cmd} {}", params.join(" "));
            let error = handle_admin_cmd(&cmd_line, username, state).await;
            if let Some(err) = error {
                // Permission denied or invalid args — send error back privately.
                let sys = make_system(&state.room_id, "broker", err);
                let json = serde_json::to_string(&sys)?;
                return Ok(CommandResult::Reply(json));
            }
            // Admin command succeeded — the command itself may have broadcast a notice.
            // If it was /exit the shutdown signal was already sent; signal the caller.
            if cmd == "exit" {
                return Ok(CommandResult::Shutdown);
            }
            return Ok(CommandResult::Handled);
        }

        // Plugin dispatch — check registry before falling through to Passthrough.
        if let Some(plugin) = state.plugin_registry.resolve(cmd) {
            let plugin_name = plugin.name().to_owned();
            match dispatch_plugin(plugin, &msg, username, state).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // Plugin errors are sent as private replies, never swallowed.
                    let err_msg = format!("plugin:{plugin_name} error: {e}");
                    let sys = make_system(&state.room_id, "broker", err_msg);
                    let json = serde_json::to_string(&sys)?;
                    return Ok(CommandResult::Reply(json));
                }
            }
        }
    }

    Ok(CommandResult::Passthrough(msg))
}

/// Validate `params` against a command's [`CommandInfo`] schema.
///
/// Returns `Ok(())` if all constraints pass, or `Err(message)` with a
/// human-readable error suitable for sending back as a reply.
///
/// Validation rules:
/// - Required params must be present (not blank).
/// - `ParamType::Choice` values must be in the allowed set.
/// - `ParamType::Number` values must parse as `i64` and respect min/max bounds.
/// - `ParamType::Text` and `ParamType::Username` are accepted as-is (no
///   server-side validation — username existence is not checked here).
fn validate_params(params: &[String], schema: &CommandInfo) -> Result<(), String> {
    for (i, ps) in schema.params.iter().enumerate() {
        let value = params.get(i).map(String::as_str).unwrap_or("");
        if ps.required && value.is_empty() {
            return Err(format!(
                "/{}: missing required parameter <{}>",
                schema.name, ps.name
            ));
        }
        if value.is_empty() {
            continue;
        }
        match &ps.param_type {
            ParamType::Choice(allowed) => {
                if !allowed.iter().any(|a| a == value) {
                    return Err(format!(
                        "/{}: <{}> must be one of: {}",
                        schema.name,
                        ps.name,
                        allowed.join(", ")
                    ));
                }
            }
            ParamType::Number { min, max } => {
                let Ok(n) = value.parse::<i64>() else {
                    return Err(format!(
                        "/{}: <{}> must be a number, got '{}'",
                        schema.name, ps.name, value
                    ));
                };
                if let Some(lo) = min {
                    if n < *lo {
                        return Err(format!("/{}: <{}> must be >= {lo}", schema.name, ps.name));
                    }
                }
                if let Some(hi) = max {
                    if n > *hi {
                        return Err(format!("/{}: <{}> must be <= {hi}", schema.name, ps.name));
                    }
                }
            }
            ParamType::Text | ParamType::Username => {}
        }
    }
    Ok(())
}

/// Build a [`CommandContext`] and call a plugin's `handle` method, translating
/// the [`PluginResult`] into a [`CommandResult`] the broker understands.
async fn dispatch_plugin(
    plugin: &dyn crate::plugin::Plugin,
    msg: &Message,
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    let (cmd, params, id, ts) = match msg {
        Message::Command {
            cmd,
            params,
            id,
            ts,
            ..
        } => (cmd, params, id, ts),
        _ => return Ok(CommandResult::Passthrough(msg.clone())),
    };

    // Schema validation — check params against the plugin's declared schema
    // before invoking the handler.
    if let Some(cmd_info) = plugin.commands().iter().find(|c| c.name == *cmd) {
        if let Err(err_msg) = validate_params(params, cmd_info) {
            let sys = make_system(
                &state.room_id,
                &format!("plugin:{}", plugin.name()),
                err_msg,
            );
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    }

    let history = HistoryReader::new(&state.chat_path, username);
    let writer = ChatWriter::new(
        &state.clients,
        &state.chat_path,
        &state.room_id,
        &state.seq_counter,
        plugin.name(),
    );
    let metadata =
        RoomMetadata::snapshot(&state.status_map, &state.host_user, &state.chat_path).await;
    let available_commands = state.plugin_registry.all_commands();

    let ctx = CommandContext {
        command: cmd.clone(),
        params: params.clone(),
        sender: username.to_owned(),
        room_id: state.room_id.as_ref().clone(),
        message_id: id.clone(),
        timestamp: *ts,
        history,
        writer,
        metadata,
        available_commands,
    };

    let result = plugin.handle(ctx).await?;

    Ok(match result {
        PluginResult::Reply(text) => {
            let sys = make_system(&state.room_id, &format!("plugin:{}", plugin.name()), text);
            let json = serde_json::to_string(&sys)?;
            CommandResult::Reply(json)
        }
        PluginResult::Broadcast(text) => {
            let sys = make_system(&state.room_id, &format!("plugin:{}", plugin.name()), text);
            broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter)
                .await?;
            CommandResult::Handled
        }
        PluginResult::Handled => CommandResult::Handled,
    })
}

/// Dispatch a `\command [arg]` line sent from a connected client.
///
/// Returns `None` on success or `Some(error_message)` if the command was rejected.
/// The caller is responsible for delivering any error message back to the issuer.
///
/// Only the room host (the first user to complete the interactive join handshake) is
/// authorised to run admin commands. All other callers receive a permission denied error.
///
/// Supported commands:
/// - `/kick <username>`      — invalidates the user's token so they cannot issue further
///   authenticated requests; the username remains reserved so they cannot rejoin without `\reauth`.
///   Also removes them from the status map so `/who` no longer shows them as online.
/// - `/reauth <username>`    — removes the user's token entirely so they can `room join` again.
/// - `/clear-tokens`         — removes every token for this room (all users must rejoin).
/// - `/exit`                 — broadcasts a shutdown notice then signals the broker to stop.
/// - `/clear`                — truncates the chat history file and broadcasts a notice.
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
    let status_map = &state.status_map;
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
            status_map.lock().await.remove(&target);
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

// ── Room management commands ──────────────────────────────────────────────────

/// Handle `/room-info` — display room visibility, config, and member count.
async fn handle_room_info(state: &RoomState) -> String {
    let member_count = state.status_map.lock().await.len();
    match &state.config {
        Some(config) => {
            let vis = serde_json::to_string(&config.visibility).unwrap_or_default();
            let max = config
                .max_members
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unlimited".to_owned());
            let invites: Vec<&str> = config.invite_list.iter().map(|s| s.as_str()).collect();
            format!(
                "room: {} | visibility: {} | max members: {} | members online: {} | invited: [{}] | created by: {}",
                state.room_id, vis, max, member_count, invites.join(", "), config.created_by
            )
        }
        None => {
            format!(
                "room: {} | visibility: public (legacy) | members online: {}",
                state.room_id, member_count
            )
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{handle_admin_cmd, handle_room_info, route_command, CommandResult};
    use crate::{
        broker::state::RoomState,
        message::{make_command, make_dm, make_message},
    };
    use std::{
        collections::HashMap,
        sync::{atomic::AtomicU64, Arc},
    };
    use tempfile::NamedTempFile;
    use tokio::sync::{watch, Mutex};

    fn make_state(chat_path: std::path::PathBuf) -> Arc<RoomState> {
        let (shutdown_tx, _) = watch::channel(false);
        Arc::new(RoomState {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            token_map: Arc::new(Mutex::new(HashMap::new())),
            claim_map: Arc::new(Mutex::new(HashMap::new())),
            chat_path: Arc::new(chat_path),
            room_id: Arc::new("test-room".to_owned()),
            shutdown: Arc::new(shutdown_tx),
            seq_counter: Arc::new(AtomicU64::new(0)),
            plugin_registry: Arc::new(crate::plugin::PluginRegistry::new()),
            config: None,
        })
    }

    // ── route_command: passthrough ─────────────────────────────────────────

    #[tokio::test]
    async fn route_command_regular_message_is_passthrough() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_message("test-room", "alice", "hello");
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Passthrough(_)));
    }

    #[tokio::test]
    async fn route_command_dm_message_is_passthrough() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_dm("test-room", "alice", "bob", "secret");
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Passthrough(_)));
    }

    // ── route_command: set_status ──────────────────────────────────────────

    #[tokio::test]
    async fn route_command_set_status_returns_handled_with_reply_and_updates_map() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "set_status", vec!["busy".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply, got Handled or other");
        };
        assert!(
            json.contains("set status"),
            "reply JSON should contain status announcement"
        );
        assert!(
            json.contains("busy"),
            "reply JSON should contain the status text"
        );
        assert_eq!(
            state
                .status_map
                .lock()
                .await
                .get("alice")
                .map(String::as_str),
            Some("busy")
        );
    }

    #[tokio::test]
    async fn route_command_set_status_empty_params_clears_status() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state
            .status_map
            .lock()
            .await
            .insert("alice".to_owned(), "busy".to_owned());
        let msg = make_command("test-room", "alice", "set_status", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::HandledWithReply(_)));
        assert_eq!(
            state
                .status_map
                .lock()
                .await
                .get("alice")
                .map(String::as_str),
            Some("")
        );
    }

    // ── route_command: who ─────────────────────────────────────────────────

    #[tokio::test]
    async fn route_command_who_with_online_user_in_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state
            .status_map
            .lock()
            .await
            .insert("alice".to_owned(), String::new());
        let msg = make_command("test-room", "alice", "who", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        assert!(json.contains("alice"), "reply should list alice");
    }

    #[tokio::test]
    async fn route_command_who_empty_room_says_no_users_online() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "who", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        assert!(json.contains("no users online"));
    }

    #[tokio::test]
    async fn route_command_who_shows_status_alongside_name() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state
            .status_map
            .lock()
            .await
            .insert("alice".to_owned(), "reviewing PR".to_owned());
        let msg = make_command("test-room", "alice", "who", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("reviewing PR"));
    }

    // ── route_command: admin permission gating ────────────────────────────

    #[tokio::test]
    async fn route_command_admin_as_non_host_gets_permission_denied_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("host-user".to_owned());
        let msg = make_command("test-room", "alice", "kick", vec!["bob".to_owned()]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("permission denied"));
    }

    #[tokio::test]
    async fn route_command_admin_when_no_host_set_gets_permission_denied() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // host_user is None
        let msg = make_command("test-room", "alice", "exit", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("permission denied"));
    }

    // ── route_command: admin commands as host ─────────────────────────────

    #[tokio::test]
    async fn route_command_kick_as_host_returns_handled_and_invalidates_token() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        state
            .token_map
            .lock()
            .await
            .insert("some-uuid".to_owned(), "bob".to_owned());
        let msg = make_command("test-room", "alice", "kick", vec!["bob".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Handled));
        let guard = state.token_map.lock().await;
        assert!(
            !guard.contains_key("some-uuid"),
            "original token must be revoked"
        );
        assert_eq!(
            guard.get("KICKED:bob").map(String::as_str),
            Some("bob"),
            "KICKED sentinel must be inserted"
        );
    }

    #[tokio::test]
    async fn route_command_exit_as_host_returns_shutdown() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let msg = make_command("test-room", "alice", "exit", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Shutdown));
    }

    // ── handle_admin_cmd directly ─────────────────────────────────────────

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

    // ── route_command: built-in param validation ────────────────────────

    #[tokio::test]
    async fn route_command_kick_missing_user_gets_validation_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let msg = make_command("test-room", "alice", "kick", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply with validation error");
        };
        assert!(
            json.contains("missing required"),
            "should report missing param"
        );
        assert!(json.contains("<user>"), "should name the missing param");
    }

    #[tokio::test]
    async fn route_command_reauth_missing_user_gets_validation_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let msg = make_command("test-room", "alice", "reauth", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply with validation error");
        };
        assert!(json.contains("missing required"));
    }

    #[tokio::test]
    async fn route_command_kick_with_valid_params_passes_validation() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        // Kick with valid username — should not be rejected by validation.
        let msg = make_command("test-room", "alice", "kick", vec!["bob".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        // kick succeeds (Handled), not a validation error Reply
        assert!(matches!(result, CommandResult::Handled));
    }

    #[tokio::test]
    async fn route_command_claim_missing_task_gets_validation_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // /claim has a required "task" param
        let msg = make_command("test-room", "alice", "claim", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply with validation error");
        };
        assert!(json.contains("missing required"));
        assert!(json.contains("<task>"));
    }

    #[tokio::test]
    async fn route_command_claim_with_task_is_handled() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "claim", vec!["fix bug".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply for /claim");
        };
        assert!(json.contains("alice claimed: fix bug"));
    }

    #[tokio::test]
    async fn route_command_who_no_params_passes_validation() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // /who has no required params — should always pass validation
        let msg = make_command("test-room", "alice", "who", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Reply(_)));
    }

    #[tokio::test]
    async fn route_command_reply_missing_params_gets_validation_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // /reply requires both id and message
        let msg = make_command("test-room", "alice", "reply", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply with validation error");
        };
        assert!(json.contains("missing required"));
    }

    #[tokio::test]
    async fn route_command_nonbuiltin_command_skips_validation() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // A command not in builtin_command_infos — no schema to validate against
        let msg = make_command("test-room", "alice", "unknown_cmd", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        // Falls through to Passthrough (no schema, no handler)
        assert!(matches!(result, CommandResult::Passthrough(_)));
    }

    // ── validate_params tests ─────────────────────────────────────────────

    mod validation_tests {
        use super::super::validate_params;
        use crate::plugin::{CommandInfo, ParamSchema, ParamType};

        fn cmd_with_params(params: Vec<ParamSchema>) -> CommandInfo {
            CommandInfo {
                name: "test".to_owned(),
                description: "test".to_owned(),
                usage: "/test".to_owned(),
                params,
            }
        }

        #[test]
        fn validate_empty_schema_always_passes() {
            let cmd = cmd_with_params(vec![]);
            assert!(validate_params(&[], &cmd).is_ok());
            assert!(validate_params(&["extra".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_required_param_missing() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "user".to_owned(),
                param_type: ParamType::Text,
                required: true,
                description: "target user".to_owned(),
            }]);
            let err = validate_params(&[], &cmd).unwrap_err();
            assert!(err.contains("missing required"));
            assert!(err.contains("<user>"));
        }

        #[test]
        fn validate_required_param_present() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "user".to_owned(),
                param_type: ParamType::Text,
                required: true,
                description: "target user".to_owned(),
            }]);
            assert!(validate_params(&["alice".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_optional_param_missing_is_ok() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: None,
                    max: None,
                },
                required: false,
                description: "count".to_owned(),
            }]);
            assert!(validate_params(&[], &cmd).is_ok());
        }

        #[test]
        fn validate_choice_valid_value() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "color".to_owned(),
                param_type: ParamType::Choice(vec!["red".to_owned(), "blue".to_owned()]),
                required: true,
                description: "pick a color".to_owned(),
            }]);
            assert!(validate_params(&["red".to_owned()], &cmd).is_ok());
            assert!(validate_params(&["blue".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_choice_invalid_value() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "color".to_owned(),
                param_type: ParamType::Choice(vec!["red".to_owned(), "blue".to_owned()]),
                required: true,
                description: "pick a color".to_owned(),
            }]);
            let err = validate_params(&["green".to_owned()], &cmd).unwrap_err();
            assert!(err.contains("must be one of"));
            assert!(err.contains("red"));
            assert!(err.contains("blue"));
        }

        #[test]
        fn validate_number_valid() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: Some(1),
                    max: Some(100),
                },
                required: true,
                description: "count".to_owned(),
            }]);
            assert!(validate_params(&["50".to_owned()], &cmd).is_ok());
            assert!(validate_params(&["1".to_owned()], &cmd).is_ok());
            assert!(validate_params(&["100".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_number_not_a_number() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: None,
                    max: None,
                },
                required: true,
                description: "count".to_owned(),
            }]);
            let err = validate_params(&["abc".to_owned()], &cmd).unwrap_err();
            assert!(err.contains("must be a number"));
        }

        #[test]
        fn validate_number_below_min() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: Some(10),
                    max: None,
                },
                required: true,
                description: "count".to_owned(),
            }]);
            let err = validate_params(&["5".to_owned()], &cmd).unwrap_err();
            assert!(err.contains("must be >= 10"));
        }

        #[test]
        fn validate_number_above_max() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "count".to_owned(),
                param_type: ParamType::Number {
                    min: None,
                    max: Some(50),
                },
                required: true,
                description: "count".to_owned(),
            }]);
            let err = validate_params(&["100".to_owned()], &cmd).unwrap_err();
            assert!(err.contains("must be <= 50"));
        }

        #[test]
        fn validate_text_always_passes() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "msg".to_owned(),
                param_type: ParamType::Text,
                required: true,
                description: "message".to_owned(),
            }]);
            assert!(validate_params(&["anything at all".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_username_always_passes() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "user".to_owned(),
                param_type: ParamType::Username,
                required: true,
                description: "user".to_owned(),
            }]);
            assert!(validate_params(&["alice".to_owned()], &cmd).is_ok());
        }

        #[test]
        fn validate_multiple_params() {
            let cmd = cmd_with_params(vec![
                ParamSchema {
                    name: "user".to_owned(),
                    param_type: ParamType::Username,
                    required: true,
                    description: "target".to_owned(),
                },
                ParamSchema {
                    name: "count".to_owned(),
                    param_type: ParamType::Number {
                        min: Some(1),
                        max: Some(100),
                    },
                    required: false,
                    description: "count".to_owned(),
                },
            ]);
            // Both present and valid
            assert!(validate_params(&["alice".to_owned(), "50".to_owned()], &cmd).is_ok());
            // First present, second omitted (optional)
            assert!(validate_params(&["alice".to_owned()], &cmd).is_ok());
            // First missing (required)
            assert!(validate_params(&[], &cmd).is_err());
        }

        #[test]
        fn validate_choice_optional_missing_is_ok() {
            let cmd = cmd_with_params(vec![ParamSchema {
                name: "level".to_owned(),
                param_type: ParamType::Choice(vec!["low".to_owned(), "high".to_owned()]),
                required: false,
                description: "level".to_owned(),
            }]);
            assert!(validate_params(&[], &cmd).is_ok());
        }
    }

    // ── room management commands ──────────────────────────────────────────

    fn make_state_with_config(
        chat_path: std::path::PathBuf,
        config: room_protocol::RoomConfig,
    ) -> Arc<RoomState> {
        let (shutdown_tx, _) = watch::channel(false);
        Arc::new(RoomState {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_map: Arc::new(Mutex::new(HashMap::new())),
            host_user: Arc::new(Mutex::new(None)),
            token_map: Arc::new(Mutex::new(HashMap::new())),
            claim_map: Arc::new(Mutex::new(HashMap::new())),
            chat_path: Arc::new(chat_path),
            room_id: Arc::new("test-room".to_owned()),
            shutdown: Arc::new(shutdown_tx),
            seq_counter: Arc::new(AtomicU64::new(0)),
            plugin_registry: Arc::new(crate::plugin::PluginRegistry::new()),
            config: Some(config),
        })
    }

    #[tokio::test]
    async fn room_info_no_config() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let result = handle_room_info(&state).await;
        assert!(result.contains("legacy"));
        assert!(result.contains("test-room"));
    }

    #[tokio::test]
    async fn room_info_with_config() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let result = handle_room_info(&state).await;
        assert!(result.contains("dm"));
        assert!(result.contains("alice"));
    }

    #[tokio::test]
    async fn route_command_room_info_returns_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "room-info", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Reply(_)));
    }

    // ── task claim commands ──────────────────────────────────────────────

    #[tokio::test]
    async fn route_command_claim_stores_task_and_broadcasts() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "claim",
            vec!["fix bug #42".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("claimed"));
        assert!(json.contains("fix bug #42"));
        assert_eq!(
            state
                .claim_map
                .lock()
                .await
                .get("alice")
                .map(String::as_str),
            Some("fix bug #42")
        );
    }

    #[tokio::test]
    async fn route_command_claim_overwrites_previous() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg1 = make_command("test-room", "alice", "claim", vec!["task A".to_owned()]);
        route_command(msg1, "alice", &state).await.unwrap();
        let msg2 = make_command("test-room", "alice", "claim", vec!["task B".to_owned()]);
        route_command(msg2, "alice", &state).await.unwrap();
        assert_eq!(
            state
                .claim_map
                .lock()
                .await
                .get("alice")
                .map(String::as_str),
            Some("task B"),
            "new claim should overwrite the old one"
        );
    }

    #[tokio::test]
    async fn route_command_unclaim_removes_claim() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state
            .claim_map
            .lock()
            .await
            .insert("alice".to_owned(), "task A".to_owned());
        let msg = make_command("test-room", "alice", "unclaim", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("released"));
        assert!(json.contains("task A"));
        assert!(state.claim_map.lock().await.get("alice").is_none());
    }

    #[tokio::test]
    async fn route_command_unclaim_no_active_claim() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "unclaim", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("no active claim"));
    }

    #[tokio::test]
    async fn route_command_claimed_empty_board() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "claimed", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        assert!(json.contains("no active claims"));
    }

    #[tokio::test]
    async fn route_command_claimed_shows_all_claims() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        {
            let mut map = state.claim_map.lock().await;
            map.insert("alice".to_owned(), "task A".to_owned());
            map.insert("bob".to_owned(), "task B".to_owned());
        }
        let msg = make_command("test-room", "alice", "claimed", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        assert!(json.contains("alice: task A"));
        assert!(json.contains("bob: task B"));
    }

    #[tokio::test]
    async fn route_command_claimed_is_sorted() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        {
            let mut map = state.claim_map.lock().await;
            map.insert("zara".to_owned(), "z-task".to_owned());
            map.insert("alice".to_owned(), "a-task".to_owned());
        }
        let msg = make_command("test-room", "alice", "claimed", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        // alice should appear before zara in sorted output
        let alice_pos = json.find("alice: a-task").unwrap();
        let zara_pos = json.find("zara: z-task").unwrap();
        assert!(alice_pos < zara_pos, "claims should be sorted by username");
    }
}
