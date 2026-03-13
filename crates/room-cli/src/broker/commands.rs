use room_protocol::{EventFilter, SubscriptionTier};

use crate::{
    message::{make_system, Message},
    plugin::{
        builtin_command_infos, snapshot_metadata, ChatWriter, CommandContext, CommandInfo,
        HistoryReader, ParamType, PluginResult,
    },
};

use super::{
    admin::{handle_admin_cmd, ADMIN_CMD_NAMES},
    auth::check_join_permission,
    fanout::broadcast_and_persist,
    persistence::{persist_event_filters, persist_subscriptions},
    state::RoomState,
};

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
/// Thin dispatcher that matches on command name and delegates to dedicated
/// `handle_*` functions. For non-command messages (regular chat, DMs) returns
/// `CommandResult::Passthrough(msg)` so the caller can broadcast or DM it.
///
/// This is the unified entry point used by both the interactive inbound task
/// and `handle_oneshot_send`.
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

        match cmd.as_str() {
            "who" => return handle_who(state).await,
            "set_status" => return handle_set_status(params, username, state).await,
            "subscribe" | "set_subscription" => {
                return handle_subscribe(params, username, state).await
            }
            "unsubscribe" => return handle_unsubscribe(username, state).await,
            "subscribe_events" | "set_event_filter" => {
                return handle_subscribe_events(params, username, state).await
            }
            "subscriptions" => return handle_subscriptions(state).await,
            "info" | "room-info" => return handle_info_cmd(params, state).await,
            "help" => return handle_help_cmd(params, state),
            _ if ADMIN_CMD_NAMES.contains(&cmd.as_str()) => {
                return handle_admin(cmd, params, username, state).await
            }
            _ => {}
        }

        // Plugin dispatch — check registry before falling through to Passthrough.
        if let Some(plugin) = state.plugin_registry.resolve(cmd) {
            let plugin_name = plugin.name().to_owned();
            match dispatch_plugin(plugin, &msg, username, state).await {
                Ok(result) => return Ok(result),
                Err(e) => {
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

// ── Command handlers ─────────────────────────────────────────────────────────

/// Handle `/who` — list online users with their statuses.
async fn handle_who(state: &RoomState) -> anyhow::Result<CommandResult> {
    let entries: Vec<String> = state
        .status_entries()
        .await
        .into_iter()
        .map(|(u, s)| {
            if s.is_empty() {
                u
            } else {
                // Sanitize commas in status text so the TUI parser
                // (which splits on ", ") doesn't treat status fragments
                // as separate usernames (#656).
                let safe = s.replace(", ", "; ");
                format!("{u}: {safe}")
            }
        })
        .collect();
    let content = if entries.is_empty() {
        "no users online".to_owned()
    } else {
        format!("online — {}", entries.join(", "))
    };
    let sys = make_system(&state.room_id, "broker", content);
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::Reply(json))
}

/// Handle `/set_status [text]` — set or clear the user's status, broadcast the change.
async fn handle_set_status(
    params: &[String],
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    let status = params.join(" ");
    state.set_status(username, status.clone()).await;
    let display = if status.is_empty() {
        format!("{username} cleared their status")
    } else {
        format!("{username} set status: {status}")
    };
    let sys = make_system(&state.room_id, "broker", display);
    let seq_msg =
        broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await?;
    let json = serde_json::to_string(&seq_msg)?;
    Ok(CommandResult::HandledWithReply(json))
}

/// Handle `/subscribe [tier]` — subscribe to room messages with a tier filter.
async fn handle_subscribe(
    params: &[String],
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    // Reject subscriptions from users who cannot join this room.
    // Without this guard, non-participants could subscribe to DM rooms
    // and read regular messages through the poll path (#491).
    if let Err(reason) = check_join_permission(username, state.config.as_ref()) {
        let sys = make_system(&state.room_id, "broker", reason);
        let json = serde_json::to_string(&sys)?;
        return Ok(CommandResult::Reply(json));
    }
    let tier_str = params.first().map(String::as_str).unwrap_or("full");
    let tier: SubscriptionTier = match tier_str.parse() {
        Ok(t) => t,
        Err(e) => {
            let sys = make_system(&state.room_id, "broker", e);
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    };
    state.set_subscription(username, tier).await;
    persist_subscriptions(state).await;
    let display = format!("{username} subscribed to {} (tier: {tier})", state.room_id);
    let sys = make_system(&state.room_id, "broker", display);
    broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await?;
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::HandledWithReply(json))
}

/// Handle `/unsubscribe` — set subscription to Unsubscribed tier.
async fn handle_unsubscribe(username: &str, state: &RoomState) -> anyhow::Result<CommandResult> {
    state
        .set_subscription(username, SubscriptionTier::Unsubscribed)
        .await;
    persist_subscriptions(state).await;
    let display = format!("{username} unsubscribed from {}", state.room_id);
    let sys = make_system(&state.room_id, "broker", display);
    broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await?;
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::HandledWithReply(json))
}

/// Handle `/subscribe_events [filter]` — set event filter for the user.
async fn handle_subscribe_events(
    params: &[String],
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    // Reject from users who cannot join this room (same guard as subscribe).
    if let Err(reason) = check_join_permission(username, state.config.as_ref()) {
        let sys = make_system(&state.room_id, "broker", reason);
        let json = serde_json::to_string(&sys)?;
        return Ok(CommandResult::Reply(json));
    }
    let filter_str = params.first().map(String::as_str).unwrap_or("all");
    let filter: EventFilter = match filter_str.parse() {
        Ok(f) => f,
        Err(e) => {
            let sys = make_system(&state.room_id, "broker", e);
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    };
    state.set_event_filter(username, filter.clone()).await;
    persist_event_filters(state).await;
    let display = format!(
        "{username} event filter set to {filter} in {}",
        state.room_id
    );
    let sys = make_system(&state.room_id, "broker", display);
    broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter).await?;
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::HandledWithReply(json))
}

/// Handle `/subscriptions` — list all subscription tiers and event filters.
async fn handle_subscriptions(state: &RoomState) -> anyhow::Result<CommandResult> {
    let raw = state.subscription_entries().await;
    let ef_raw = state.event_filter_entries().await;
    let content = if raw.is_empty() && ef_raw.is_empty() {
        "no subscriptions".to_owned()
    } else {
        let mut parts = Vec::new();
        if !raw.is_empty() {
            let entries: Vec<String> = raw.into_iter().map(|(u, t)| format!("{u}: {t}")).collect();
            parts.push(format!("tiers — {}", entries.join(", ")));
        }
        if !ef_raw.is_empty() {
            let entries: Vec<String> = ef_raw
                .into_iter()
                .map(|(u, f)| format!("{u}: {f}"))
                .collect();
            parts.push(format!("event filters — {}", entries.join(", ")));
        }
        parts.join(" | ")
    };
    let sys = make_system(&state.room_id, "broker", content);
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::Reply(json))
}

/// Handle `/info [username]` — dispatch to room info or user info.
async fn handle_info_cmd(params: &[String], state: &RoomState) -> anyhow::Result<CommandResult> {
    let result = handle_info(params, state).await;
    let sys = make_system(&state.room_id, "broker", result);
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::Reply(json))
}

/// Handle `/help [command]` — list commands or show detail for one.
fn handle_help_cmd(params: &[String], state: &RoomState) -> anyhow::Result<CommandResult> {
    let reply = handle_help(params, state);
    let sys = make_system(&state.room_id, "broker", reply);
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::Reply(json))
}

/// Handle admin commands (`/kick`, `/reauth`, `/clear-tokens`, `/exit`, `/clear`).
async fn handle_admin(
    cmd: &str,
    params: &[String],
    username: &str,
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    let cmd_line = format!("{cmd} {}", params.join(" "));
    let error = handle_admin_cmd(&cmd_line, username, state).await;
    if let Some(err) = error {
        let sys = make_system(&state.room_id, "broker", err);
        let json = serde_json::to_string(&sys)?;
        return Ok(CommandResult::Reply(json));
    }
    if cmd == "exit" {
        return Ok(CommandResult::Shutdown);
    }
    Ok(CommandResult::Handled)
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
    let metadata = snapshot_metadata(&state.status_map, &state.host_user, &state.chat_path).await;
    let available_commands = state.plugin_registry.all_commands();

    let ctx = CommandContext {
        command: cmd.clone(),
        params: params.clone(),
        sender: username.to_owned(),
        room_id: state.room_id.as_ref().clone(),
        message_id: id.clone(),
        timestamp: *ts,
        history: Box::new(history),
        writer: Box::new(writer),
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
            let seq_msg =
                broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter)
                    .await?;
            let json = serde_json::to_string(&seq_msg)?;
            CommandResult::HandledWithReply(json)
        }
        PluginResult::Handled => CommandResult::Handled,
    })
}

// ── Room management commands ──────────────────────────────────────────────────

/// Handle `/info [username]` — show room metadata or user info.
///
/// - `/info` (no args): room ID, visibility, config, host, member count, subscribers.
/// - `/info <username>`: status, subscription tier, host flag, online status.
///
/// `/room-info` is an alias that always shows room info (ignores params).
async fn handle_info(params: &[String], state: &RoomState) -> String {
    if let Some(target) = params.first() {
        let target = target.strip_prefix('@').unwrap_or(target);
        return handle_user_info(target, state).await;
    }
    handle_room_info(state).await
}

/// Room metadata: visibility, config, host, member count, subscribers.
async fn handle_room_info(state: &RoomState) -> String {
    let member_count = state.status_count().await;
    let sub_count = state.subscription_count().await;
    let host = state
        .host_user
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| "(none)".to_owned());

    match &state.config {
        Some(config) => {
            let vis = serde_json::to_string(&config.visibility).unwrap_or_default();
            let max = config
                .max_members
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unlimited".to_owned());
            let invites: Vec<&str> = config.invite_list.iter().map(|s| s.as_str()).collect();
            format!(
                "room: {} | visibility: {} | max members: {} | host: {} | members online: {} | subscribers: {} | invited: [{}] | created by: {}",
                state.room_id, vis, max, host, member_count, sub_count, invites.join(", "), config.created_by
            )
        }
        None => {
            format!(
                "room: {} | visibility: public (legacy) | host: {} | members online: {} | subscribers: {}",
                state.room_id, host, member_count, sub_count
            )
        }
    }
}

/// User info: online status, status text, subscription tier, host flag.
async fn handle_user_info(target: &str, state: &RoomState) -> String {
    let status_map = state.status_map.lock().await;
    let online = status_map.contains_key(target);
    let status_text = status_map.get(target).cloned().unwrap_or_default();
    drop(status_map);

    let sub_tier = state
        .filters
        .subscription_map
        .lock()
        .await
        .get(target)
        .copied();

    let is_host = state.host_user.lock().await.as_deref() == Some(target);

    let mut parts = vec![format!("user: {target}")];

    if online {
        if status_text.is_empty() {
            parts.push("online".to_owned());
        } else {
            parts.push(format!("online ({status_text})"));
        }
    } else {
        parts.push("offline".to_owned());
    }

    if let Some(tier) = sub_tier {
        parts.push(format!("subscription: {tier}"));
    } else {
        parts.push("subscription: none".to_owned());
    }

    if is_host {
        parts.push("host: yes".to_owned());
    }

    parts.join(" | ")
}

// ── Help command ──────────────────────────────────────────────────────────────

/// Handle `/help [command]` — lists all commands or shows detail for one.
fn handle_help(params: &[String], state: &RoomState) -> String {
    let builtins = builtin_command_infos();
    let plugin_cmds = state.plugin_registry.all_commands();

    if let Some(target) = params.first() {
        let target = target.strip_prefix('/').unwrap_or(target);

        // Check plugin commands first (matches original behaviour)
        if let Some(cmd) = plugin_cmds.iter().find(|c| c.name == target) {
            return format_command_help(cmd);
        }
        // Then builtins
        if let Some(cmd) = builtins.iter().find(|c| c.name == target) {
            return format_command_help(cmd);
        }
        return format!("unknown command: /{target}");
    }

    // List all commands: builtins first, then plugins
    let mut lines = vec!["available commands:".to_owned()];
    for cmd in &builtins {
        lines.push(format!("  {} — {}", cmd.usage, cmd.description));
    }
    for cmd in &plugin_cmds {
        lines.push(format!("  {} — {}", cmd.usage, cmd.description));
    }
    lines.join("\n")
}

/// Format detailed help for a single command, including typed parameter info.
fn format_command_help(cmd: &CommandInfo) -> String {
    let mut lines = vec![cmd.usage.clone(), format!("  {}", cmd.description)];
    if !cmd.params.is_empty() {
        lines.push("  parameters:".to_owned());
        for p in &cmd.params {
            let req = if p.required { "required" } else { "optional" };
            let type_hint = match &p.param_type {
                ParamType::Text => "text".to_owned(),
                ParamType::Username => "username".to_owned(),
                ParamType::Number { min, max } => match (min, max) {
                    (Some(lo), Some(hi)) => format!("number ({lo}..{hi})"),
                    (Some(lo), None) => format!("number ({lo}..)"),
                    (None, Some(hi)) => format!("number (..{hi})"),
                    (None, None) => "number".to_owned(),
                },
                ParamType::Choice(values) => {
                    format!("one of: {}", values.join(", "))
                }
            };
            lines.push(format!(
                "    <{}> — {} [{}] {}",
                p.name, p.description, req, type_hint
            ));
        }
    }
    lines.join("\n")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{handle_info, handle_room_info, handle_user_info, route_command, CommandResult};
    use crate::{
        broker::{
            persistence::{
                load_event_filter_map, load_subscription_map, save_event_filter_map,
                save_subscription_map,
            },
            state::RoomState,
        },
        message::{make_command, make_dm, make_message},
    };
    use room_protocol::SubscriptionTier;
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

    #[tokio::test]
    async fn route_command_set_status_multi_word_joins_all_params() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "set_status",
            vec!["reviewing".to_owned(), "PR".to_owned(), "#42".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(
            json.contains("reviewing PR #42"),
            "broadcast must contain full multi-word status, got: {json}"
        );
        assert_eq!(
            state
                .status_map
                .lock()
                .await
                .get("alice")
                .map(String::as_str),
            Some("reviewing PR #42")
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

    #[tokio::test]
    async fn route_command_who_sanitizes_commas_in_status() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        {
            let mut map = state.status_map.lock().await;
            map.insert("alice".to_owned(), "PR #630 merged, #636 filed".to_owned());
            map.insert("bob".to_owned(), String::new());
        }
        let msg = make_command("test-room", "alice", "who", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        // The comma in the status must be replaced so the TUI parser
        // doesn't treat "#636 filed" as a separate username (#656).
        assert!(
            !json.contains("PR #630 merged, #636"),
            "raw comma must be sanitized: {json}"
        );
        assert!(
            json.contains("PR #630 merged; #636 filed"),
            "comma should be replaced with semicolon: {json}"
        );
        // bob should still appear as a separate entry
        assert!(json.contains("bob"), "bob should be listed: {json}");
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
            .auth
            .token_map
            .lock()
            .await
            .insert("some-uuid".to_owned(), "bob".to_owned());
        let msg = make_command("test-room", "alice", "kick", vec!["bob".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Handled));
        let guard = state.auth.token_map.lock().await;
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
        let token_map_path = chat_path.with_extension("tokens");
        let subscription_map_path = chat_path.with_extension("subscriptions");
        RoomState::new(
            "test-room".to_owned(),
            chat_path,
            token_map_path,
            subscription_map_path,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Some(config),
        )
        .unwrap()
    }

    // ── /info and /room-info ─────────────────────────────────────────────

    #[tokio::test]
    async fn room_info_no_config() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let result = handle_room_info(&state).await;
        assert!(result.contains("legacy"));
        assert!(result.contains("test-room"));
    }

    #[tokio::test]
    async fn room_info_includes_host() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        let result = handle_room_info(&state).await;
        assert!(result.contains("host: alice"), "got: {result}");
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
    async fn info_no_args_shows_room_info() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let result = handle_info(&[], &state).await;
        assert!(result.contains("test-room"), "got: {result}");
        assert!(result.contains("legacy"), "got: {result}");
    }

    #[tokio::test]
    async fn info_with_username_shows_user_info() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.set_status("bob", "coding".to_owned()).await;
        state.set_subscription("bob", SubscriptionTier::Full).await;
        let result = handle_info(&["bob".to_owned()], &state).await;
        assert!(result.contains("user: bob"), "got: {result}");
        assert!(result.contains("online (coding)"), "got: {result}");
        assert!(result.contains("subscription: full"), "got: {result}");
    }

    #[tokio::test]
    async fn info_strips_at_prefix() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.set_status("carol", String::new()).await;
        let result = handle_info(&["@carol".to_owned()], &state).await;
        assert!(result.contains("user: carol"), "got: {result}");
    }

    #[tokio::test]
    async fn user_info_offline_user() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let result = handle_user_info("ghost", &state).await;
        assert!(result.contains("user: ghost"), "got: {result}");
        assert!(result.contains("offline"), "got: {result}");
        assert!(result.contains("subscription: none"), "got: {result}");
    }

    #[tokio::test]
    async fn user_info_online_no_status() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.set_status("alice", String::new()).await;
        let result = handle_user_info("alice", &state).await;
        assert!(result.contains("online"), "got: {result}");
        assert!(!result.contains("offline"), "got: {result}");
    }

    #[tokio::test]
    async fn user_info_shows_host_flag() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        state.set_status("alice", "hosting".to_owned()).await;
        let result = handle_user_info("alice", &state).await;
        assert!(result.contains("host: yes"), "got: {result}");
    }

    #[tokio::test]
    async fn user_info_non_host_omits_host_flag() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        *state.host_user.lock().await = Some("alice".to_owned());
        state.set_status("bob", String::new()).await;
        let result = handle_user_info("bob", &state).await;
        assert!(!result.contains("host"), "got: {result}");
    }

    #[tokio::test]
    async fn route_command_info_returns_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "info", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Reply(_)));
    }

    #[tokio::test]
    async fn route_command_room_info_alias_returns_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "room-info", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(matches!(result, CommandResult::Reply(_)));
    }

    #[tokio::test]
    async fn route_command_info_with_user_returns_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        state.set_status("bob", "busy".to_owned()).await;
        let msg = make_command("test-room", "alice", "info", vec!["bob".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        match result {
            CommandResult::Reply(json) => {
                assert!(json.contains("user: bob"), "got: {json}");
                assert!(json.contains("online (busy)"), "got: {json}");
            }
            _ => panic!("expected Reply"),
        }
    }

    // ── subscription commands ──────────────────────────────────────────────

    #[tokio::test]
    async fn set_subscription_alias_works() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "set_subscription",
            vec!["full".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("subscribed"));
        assert!(json.contains("full"));
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
    async fn set_subscription_alias_mentions_only() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "bob",
            "set_subscription",
            vec!["mentions_only".to_owned()],
        );
        let result = route_command(msg, "bob", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("subscribed"));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("bob")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn subscribe_default_tier_is_full() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("subscribed"));
        assert!(json.contains("full"));
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
    async fn subscribe_explicit_mentions_only() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "bob",
            "subscribe",
            vec!["mentions_only".to_owned()],
        );
        let result = route_command(msg, "bob", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("mentions_only"));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("bob")
                .unwrap(),
            SubscriptionTier::MentionsOnly
        );
    }

    #[tokio::test]
    async fn subscribe_overwrites_previous_tier() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg1 = make_command("test-room", "alice", "subscribe", vec!["full".to_owned()]);
        route_command(msg1, "alice", &state).await.unwrap();
        let msg2 = make_command(
            "test-room",
            "alice",
            "subscribe",
            vec!["mentions_only".to_owned()],
        );
        route_command(msg2, "alice", &state).await.unwrap();
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::MentionsOnly,
            "second subscribe should overwrite the first"
        );
    }

    #[tokio::test]
    async fn unsubscribe_sets_tier_to_unsubscribed() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // Subscribe first
        let msg = make_command("test-room", "alice", "subscribe", vec!["full".to_owned()]);
        route_command(msg, "alice", &state).await.unwrap();
        // Then unsubscribe
        let msg = make_command("test-room", "alice", "unsubscribe", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("unsubscribed"));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::Unsubscribed
        );
    }

    #[tokio::test]
    async fn unsubscribe_without_prior_subscription() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "unsubscribe", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        // Should still work — sets to Unsubscribed even without prior entry
        assert!(matches!(result, CommandResult::HandledWithReply(_)));
        assert_eq!(
            *state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .unwrap(),
            SubscriptionTier::Unsubscribed
        );
    }

    #[tokio::test]
    async fn subscriptions_empty() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscriptions", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("no subscriptions"));
    }

    #[tokio::test]
    async fn subscriptions_lists_all_sorted() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        {
            let mut map = state.filters.subscription_map.lock().await;
            map.insert("zara".to_owned(), SubscriptionTier::Full);
            map.insert("alice".to_owned(), SubscriptionTier::MentionsOnly);
        }
        let msg = make_command("test-room", "alice", "subscriptions", vec![]);
        let CommandResult::Reply(json) = route_command(msg, "alice", &state).await.unwrap() else {
            panic!("expected Reply");
        };
        assert!(json.contains("alice: mentions_only"));
        assert!(json.contains("zara: full"));
        // Verify sorted order
        let alice_pos = json.find("alice: mentions_only").unwrap();
        let zara_pos = json.find("zara: full").unwrap();
        assert!(
            alice_pos < zara_pos,
            "subscriptions should be sorted by username"
        );
    }

    #[tokio::test]
    async fn subscribe_invalid_tier_returns_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe", vec!["banana".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for invalid tier");
        };
        assert!(json.contains("must be one of"));
        // Should not have stored anything
        assert!(state
            .filters
            .subscription_map
            .lock()
            .await
            .get("alice")
            .is_none());
    }

    #[tokio::test]
    async fn subscribe_broadcasts_system_message() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        // Verify the broadcast was persisted to chat history
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("subscribed"));
        assert!(history.contains("alice"));
    }

    #[tokio::test]
    async fn unsubscribe_broadcasts_system_message() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "unsubscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("unsubscribed"));
        assert!(history.contains("alice"));
    }

    // ── subscribe join-permission guard (#491) ─────────────────────────

    #[tokio::test]
    async fn subscribe_dm_room_participant_allowed() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        assert!(
            matches!(result, CommandResult::HandledWithReply(_)),
            "DM participant should be allowed to subscribe"
        );
        assert_eq!(
            state
                .filters
                .subscription_map
                .lock()
                .await
                .get("alice")
                .copied(),
            Some(SubscriptionTier::Full),
        );
    }

    #[tokio::test]
    async fn subscribe_dm_room_non_participant_rejected() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let msg = make_command("test-room", "eve", "subscribe", vec![]);
        let result = route_command(msg, "eve", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply (rejection) for non-participant");
        };
        assert!(
            json.contains("permission denied"),
            "should contain permission denied, got: {json}"
        );
        assert!(
            state
                .filters
                .subscription_map
                .lock()
                .await
                .get("eve")
                .is_none(),
            "non-participant must not get a subscription entry"
        );
    }

    #[tokio::test]
    async fn subscribe_private_room_non_invited_rejected() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig {
            visibility: room_protocol::RoomVisibility::Private,
            max_members: None,
            invite_list: ["alice".to_owned()].into(),
            created_by: "owner".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let msg = make_command("test-room", "stranger", "subscribe", vec![]);
        let result = route_command(msg, "stranger", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply (rejection)");
        };
        assert!(json.contains("permission denied"));
        assert!(state
            .filters
            .subscription_map
            .lock()
            .await
            .get("stranger")
            .is_none());
    }

    #[tokio::test]
    async fn subscribe_public_room_always_allowed() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::public("owner");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        let msg = make_command("test-room", "anyone", "subscribe", vec![]);
        let result = route_command(msg, "anyone", &state).await.unwrap();
        assert!(
            matches!(result, CommandResult::HandledWithReply(_)),
            "public room subscribe should succeed"
        );
    }

    #[tokio::test]
    async fn set_subscription_alias_dm_guard_applies() {
        let tmp = NamedTempFile::new().unwrap();
        let config = room_protocol::RoomConfig::dm("alice", "bob");
        let state = make_state_with_config(tmp.path().to_path_buf(), config);
        // set_subscription is an alias for subscribe — guard must apply
        let msg = make_command(
            "test-room",
            "eve",
            "set_subscription",
            vec!["full".to_owned()],
        );
        let result = route_command(msg, "eve", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply (rejection) for non-participant via alias");
        };
        assert!(json.contains("permission denied"));
    }

    // ── subscription persistence ─────────────────────────────────────────

    #[test]
    fn load_subscription_map_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.subscriptions");
        let map = load_subscription_map(&path);
        assert!(map.is_empty());
    }

    #[test]
    fn save_and_load_subscription_map_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.subscriptions");

        let mut original = HashMap::new();
        original.insert("alice".to_owned(), SubscriptionTier::Full);
        original.insert("bob".to_owned(), SubscriptionTier::MentionsOnly);
        original.insert("carol".to_owned(), SubscriptionTier::Unsubscribed);

        save_subscription_map(&original, &path).unwrap();
        let loaded = load_subscription_map(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_subscription_map_corrupt_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.subscriptions");
        std::fs::write(&path, "not json{{{").unwrap();
        let map = load_subscription_map(&path);
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn subscribe_persists_to_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();

        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::Full));
    }

    #[tokio::test]
    async fn unsubscribe_persists_to_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());

        // Subscribe first, then unsubscribe.
        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        let msg = make_command("test-room", "alice", "unsubscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();

        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::Unsubscribed));
    }

    #[tokio::test]
    async fn subscribe_accumulates_on_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());

        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        let msg = make_command(
            "test-room",
            "bob",
            "subscribe",
            vec!["mentions_only".to_owned()],
        );
        route_command(msg, "bob", &state).await.unwrap();

        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::Full));
        assert_eq!(loaded.get("bob"), Some(&SubscriptionTier::MentionsOnly));
    }

    #[tokio::test]
    async fn subscribe_survives_simulated_restart() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());

        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();

        // Simulate restart: new state, load from disk.
        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::Full));

        // Verify it can be populated into a new RoomState.
        let state2 = RoomState::new(
            state.room_id.as_ref().clone(),
            state.chat_path.as_ref().clone(),
            state.auth.token_map_path.as_ref().clone(),
            state.filters.subscription_map_path.as_ref().clone(),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(loaded)),
            None,
        )
        .unwrap();
        assert_eq!(
            *state2
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
    async fn subscribe_overwrite_persists_latest_tier() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());

        let msg = make_command("test-room", "alice", "subscribe", vec![]);
        route_command(msg, "alice", &state).await.unwrap();
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe",
            vec!["mentions_only".to_owned()],
        );
        route_command(msg, "alice", &state).await.unwrap();

        let loaded = load_subscription_map(&state.filters.subscription_map_path);
        assert_eq!(loaded.get("alice"), Some(&SubscriptionTier::MentionsOnly));
    }

    // ── subscribe_events command ────────────────────────────────────────

    fn make_state_with_event_filters(chat_path: std::path::PathBuf) -> Arc<RoomState> {
        let state = make_state(chat_path.clone());
        let ef_path = chat_path.with_extension("event_filters");
        state.set_event_filter_map(Arc::new(Mutex::new(HashMap::new())), ef_path);
        state
    }

    #[tokio::test]
    async fn subscribe_events_all() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["all".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("event filter"));
        assert!(json.contains("all"));
    }

    #[tokio::test]
    async fn subscribe_events_none() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["none".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("event filter"));
        assert!(json.contains("none"));
    }

    #[tokio::test]
    async fn subscribe_events_csv() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["task_posted,task_finished".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("event filter"));
        assert!(json.contains("task_finished"));
        assert!(json.contains("task_posted"));
    }

    #[tokio::test]
    async fn subscribe_events_invalid_type_returns_error() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["banana".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for invalid event type");
        };
        assert!(json.contains("unknown event type"));
    }

    #[tokio::test]
    async fn subscribe_events_default_is_all() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "subscribe_events", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply");
        };
        assert!(json.contains("event filter"));
        assert!(json.contains("all"));
    }

    #[tokio::test]
    async fn subscribe_events_broadcasts_system_message() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["task_posted".to_owned()],
        );
        route_command(msg, "alice", &state).await.unwrap();
        let history = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(history.contains("event filter"));
        assert!(history.contains("alice"));
    }

    #[tokio::test]
    async fn subscribe_events_persists_to_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());
        let msg = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["task_posted".to_owned()],
        );
        route_command(msg, "alice", &state).await.unwrap();

        let ef_path = tmp.path().with_extension("event_filters");
        let loaded = load_event_filter_map(&ef_path);
        assert!(loaded.contains_key("alice"));
    }

    #[tokio::test]
    async fn subscribe_events_overwrites_previous() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state_with_event_filters(tmp.path().to_path_buf());

        let msg1 = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["task_posted".to_owned()],
        );
        route_command(msg1, "alice", &state).await.unwrap();

        let msg2 = make_command(
            "test-room",
            "alice",
            "subscribe_events",
            vec!["none".to_owned()],
        );
        route_command(msg2, "alice", &state).await.unwrap();

        let ef_path = tmp.path().with_extension("event_filters");
        let loaded = load_event_filter_map(&ef_path);
        assert_eq!(loaded.get("alice"), Some(&room_protocol::EventFilter::None));
    }

    // ── event filter persistence ─────────────────────────────────────────

    #[test]
    fn load_event_filter_map_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.event_filters");
        let map = load_event_filter_map(&path);
        assert!(map.is_empty());
    }

    #[test]
    fn save_and_load_event_filter_map_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.event_filters");

        let mut original = HashMap::new();
        original.insert("alice".to_owned(), room_protocol::EventFilter::All);
        original.insert("bob".to_owned(), room_protocol::EventFilter::None);
        let mut types = std::collections::BTreeSet::new();
        types.insert(room_protocol::EventType::TaskPosted);
        original.insert(
            "carol".to_owned(),
            room_protocol::EventFilter::Only { types },
        );

        save_event_filter_map(&original, &path).unwrap();
        let loaded = load_event_filter_map(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_event_filter_map_corrupt_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.event_filters");
        std::fs::write(&path, "not json{{{").unwrap();
        let map = load_event_filter_map(&path);
        assert!(map.is_empty());
    }

    // ── plugin broadcast returns HandledWithReply for oneshot echo ─────

    #[tokio::test]
    async fn plugin_broadcast_returns_handled_with_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // /taskboard post produces PluginResult::Broadcast — should come
        // back as HandledWithReply so oneshot senders receive the echo.
        let msg = make_command(
            "test-room",
            "alice",
            "taskboard",
            vec!["post".to_owned(), "test task description".to_owned()],
        );
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::HandledWithReply(json) = result else {
            panic!("expected HandledWithReply for plugin broadcast");
        };
        assert!(
            json.contains("plugin:taskboard"),
            "reply should identify plugin source"
        );
        assert!(
            json.contains("test task description"),
            "reply should contain the task description"
        );
    }

    #[tokio::test]
    async fn plugin_reply_returns_reply_not_handled_with_reply() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        // /taskboard list with no tasks produces PluginResult::Reply.
        let msg = make_command("test-room", "alice", "taskboard", vec!["list".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for plugin list");
        };
        assert!(
            json.contains("plugin:taskboard"),
            "reply should identify plugin source"
        );
    }

    // ── route_command: help ───────────────────────────────────────────────

    #[tokio::test]
    async fn help_no_args_lists_all_commands() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help");
        };
        assert!(json.contains("available commands:"));
        assert!(json.contains("/who"));
        assert!(json.contains("/help"));
        // Plugin commands should also appear
        assert!(json.contains("/stats"));
    }

    #[tokio::test]
    async fn help_specific_builtin_command() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["who".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help who");
        };
        assert!(json.contains("/who"));
        assert!(json.contains("List users in the room"));
    }

    #[tokio::test]
    async fn help_specific_plugin_command() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["stats".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help stats");
        };
        assert!(json.contains("/stats"));
        assert!(json.contains("statistical summary"));
    }

    #[tokio::test]
    async fn help_unknown_command() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["nonexistent".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help nonexistent");
        };
        assert!(json.contains("unknown command: /nonexistent"));
    }

    #[tokio::test]
    async fn help_strips_leading_slash_from_arg() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["/kick".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help /kick");
        };
        assert!(json.contains("/kick"));
        assert!(json.contains("parameters:"));
        assert!(json.contains("username"));
    }

    #[tokio::test]
    async fn help_builtin_shows_param_info() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec!["kick".to_owned()]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply for /help kick");
        };
        assert!(json.contains("parameters:"));
        assert!(json.contains("username"));
        assert!(json.contains("required"));
    }

    #[tokio::test]
    async fn help_reply_comes_from_broker_not_plugin() {
        let tmp = NamedTempFile::new().unwrap();
        let state = make_state(tmp.path().to_path_buf());
        let msg = make_command("test-room", "alice", "help", vec![]);
        let result = route_command(msg, "alice", &state).await.unwrap();
        let CommandResult::Reply(json) = result else {
            panic!("expected Reply");
        };
        // Should be from "broker", not "plugin:help"
        assert!(json.contains("\"user\":\"broker\""));
        assert!(!json.contains("plugin:help"));
    }
}
