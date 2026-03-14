use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;
use room_protocol::{make_system, make_system_with_data, Message};

use crate::broker::{fanout::broadcast_and_persist, state::RoomState};
use crate::plugin::{snapshot_metadata, ChatWriter, CommandContext, HistoryReader, PluginResult};

use super::validate::validate_params;
use super::CommandResult;

/// Maximum time a plugin is allowed to run before being cancelled.
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Extract `--room <room_id>` from a params list, returning the target room ID
/// and the cleaned params with the flag and its value stripped out.
///
/// Returns `None` if no `--room` flag is present. The flag can appear at any
/// position in the params list; both it and the subsequent value are removed
/// so the plugin never sees them.
pub(super) fn extract_room_flag(params: &[String]) -> Option<(String, Vec<String>)> {
    let mut target_room = None;
    let mut cleaned = Vec::with_capacity(params.len());
    let mut skip_next = false;

    for (i, p) in params.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if p == "--room" {
            if let Some(val) = params.get(i + 1) {
                target_room = Some(val.clone());
                skip_next = true;
                continue;
            }
        }
        cleaned.push(p.clone());
    }

    target_room.map(|room_id| (room_id, cleaned))
}

/// Build a [`CommandContext`] from the given command fields and room state.
///
/// This is the single place where `CommandContext` is assembled — both
/// `dispatch_plugin` (local) and `dispatch_cross_room` (remote) call it.
pub(super) async fn build_command_context(
    cmd: &str,
    params: &[String],
    id: &str,
    ts: chrono::DateTime<chrono::Utc>,
    username: &str,
    state: &RoomState,
    plugin_name: &str,
) -> CommandContext {
    let history = HistoryReader::new(&state.chat_path, username);
    let writer = ChatWriter::new(
        &state.clients,
        &state.chat_path,
        &state.room_id,
        &state.seq_counter,
        plugin_name,
    );
    let metadata = snapshot_metadata(&state.status_map, &state.host_user, &state.chat_path).await;
    let available_commands = state.plugin_registry.all_commands();

    let team_access: Option<Box<dyn room_protocol::plugin::TeamAccess>> = state
        .auth
        .registry
        .get()
        .map(|reg| Box::new(crate::plugin::bridge::TeamChecker::new(reg)) as _);

    CommandContext {
        command: cmd.to_owned(),
        params: params.to_vec(),
        sender: username.to_owned(),
        room_id: state.room_id.as_ref().clone(),
        message_id: id.to_owned(),
        timestamp: ts,
        history: Box::new(history),
        writer: Box::new(writer),
        metadata,
        available_commands,
        team_access,
    }
}

/// Run a plugin handler with timeout and panic isolation.
///
/// Wraps the handler in [`catch_unwind`] and [`tokio::time::timeout`] so that
/// panicking or hung plugins do not take down the broker.
pub(super) async fn run_plugin_with_catch(
    plugin: &dyn crate::plugin::Plugin,
    ctx: CommandContext,
) -> anyhow::Result<PluginResult> {
    match tokio::time::timeout(
        PLUGIN_TIMEOUT,
        AssertUnwindSafe(plugin.handle(ctx)).catch_unwind(),
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(panic_info)) => {
            let msg = panic_info
                .downcast_ref::<String>()
                .map(|s| s.as_str())
                .or_else(|| panic_info.downcast_ref::<&str>().copied())
                .unwrap_or("unknown panic");
            eprintln!("[broker] plugin '{}' panicked: {msg}", plugin.name());
            Ok(PluginResult::Reply(
                format!("plugin '{}' panicked: {msg}", plugin.name()),
                None,
            ))
        }
        Err(_elapsed) => {
            eprintln!(
                "[broker] plugin '{}' timed out after {}s",
                plugin.name(),
                PLUGIN_TIMEOUT.as_secs()
            );
            Ok(PluginResult::Reply(
                format!(
                    "plugin '{}' timed out after {}s",
                    plugin.name(),
                    PLUGIN_TIMEOUT.as_secs()
                ),
                None,
            ))
        }
    }
}

/// Translate a [`PluginResult`] into a [`CommandResult`].
///
/// For cross-room dispatch, pass `cross_room = Some((source_room_id, target_room_id))`
/// so that replies are tagged with the target room prefix and broadcasts go to the
/// correct room's clients. For local dispatch, pass `None`.
pub(super) async fn translate_plugin_result(
    result: PluginResult,
    plugin_name: &str,
    state: &RoomState,
    cross_room: Option<(&str, &str)>,
) -> anyhow::Result<CommandResult> {
    Ok(match result {
        PluginResult::Reply(text, data) => {
            let reply_text = match cross_room {
                Some((_, target)) => format!("[→{target}] {text}"),
                None => text,
            };
            let reply_room = match cross_room {
                Some((source, _)) => source,
                None => &state.room_id,
            };
            let plugin_user = format!("plugin:{plugin_name}");
            let sys = match data {
                Some(d) => make_system_with_data(reply_room, &plugin_user, reply_text, d),
                None => make_system(reply_room, &plugin_user, reply_text),
            };
            let json = serde_json::to_string(&sys)?;
            CommandResult::Reply(json)
        }
        PluginResult::Broadcast(text, data) => {
            let plugin_user = format!("plugin:{plugin_name}");
            let sys = match data {
                Some(d) => make_system_with_data(&state.room_id, &plugin_user, text, d),
                None => make_system(&state.room_id, &plugin_user, text),
            };
            let seq_msg =
                broadcast_and_persist(&sys, &state.clients, &state.chat_path, &state.seq_counter)
                    .await?;
            let json = serde_json::to_string(&seq_msg)?;
            CommandResult::HandledWithReply(json)
        }
        PluginResult::Handled => CommandResult::Handled,
    })
}

/// Build a [`CommandContext`] and call a plugin's `handle` method, translating
/// the [`PluginResult`] into a [`CommandResult`] the broker understands.
///
/// If params contain `--room <id>` and the source room has a cross-room
/// resolver (daemon mode), the command is executed against the target room
/// instead of the source room. The plugin is resolved from the target room's
/// registry. This enables e.g. `/taskboard post --room other-room description`.
pub(super) async fn dispatch_plugin(
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

    // Check for --room flag to redirect to a different room.
    if let Some((target_room_id, cleaned_params)) = extract_room_flag(params) {
        return dispatch_cross_room(CrossRoomContext {
            cmd,
            params: &cleaned_params,
            id,
            ts: *ts,
            username,
            source_state: state,
            target_room_id: &target_room_id,
        })
        .await;
    }

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

    let ctx = build_command_context(cmd, params, id, *ts, username, state, plugin.name()).await;

    let result = run_plugin_with_catch(plugin, ctx).await?;

    translate_plugin_result(result, plugin.name(), state, None).await
}

/// Bundles the parameters for cross-room plugin dispatch into a single struct,
/// keeping the function signature readable and easy to extend.
struct CrossRoomContext<'a> {
    cmd: &'a str,
    params: &'a [String],
    id: &'a str,
    ts: chrono::DateTime<chrono::Utc>,
    username: &'a str,
    source_state: &'a RoomState,
    target_room_id: &'a str,
}

/// Execute a plugin command against a different room (cross-room dispatch).
///
/// Resolves the target room via `state.cross_room_resolver`, finds the plugin
/// in the target room's registry, builds a `CommandContext` pointing at the
/// target room's state, and runs the plugin handler. Broadcasts go to the
/// target room's clients and chat file.
async fn dispatch_cross_room(cx: CrossRoomContext<'_>) -> anyhow::Result<CommandResult> {
    let CrossRoomContext {
        cmd,
        params,
        id,
        ts,
        username,
        source_state,
        target_room_id,
    } = cx;

    let resolver = match source_state.cross_room_resolver.get() {
        Some(r) => r,
        None => {
            let sys = make_system(
                &source_state.room_id,
                "broker",
                "cross-room commands are only available in daemon mode",
            );
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    };

    let target_state = match resolver.resolve_room(target_room_id).await {
        Some(s) => s,
        None => {
            let sys = make_system(
                &source_state.room_id,
                "broker",
                format!("room not found: {target_room_id}"),
            );
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    };

    let plugin = match target_state.plugin_registry.resolve(cmd) {
        Some(p) => p,
        None => {
            let sys = make_system(
                &source_state.room_id,
                "broker",
                format!("command /{cmd} not found in room {target_room_id}"),
            );
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    };

    // Schema validation against the target plugin's schema.
    if let Some(cmd_info) = plugin.commands().iter().find(|c| c.name == cmd) {
        if let Err(err_msg) = validate_params(params, cmd_info) {
            let sys = make_system(
                &source_state.room_id,
                &format!("plugin:{}", plugin.name()),
                err_msg,
            );
            let json = serde_json::to_string(&sys)?;
            return Ok(CommandResult::Reply(json));
        }
    }

    // Build context against the TARGET room's state.
    let ctx =
        build_command_context(cmd, params, id, ts, username, &target_state, plugin.name()).await;

    let result = run_plugin_with_catch(plugin, ctx).await?;

    // Replies go to the SOURCE room (the caller sees them), but broadcasts go
    // to the TARGET room (where the state change happened).
    translate_plugin_result(
        result,
        plugin.name(),
        &target_state,
        Some((&source_state.room_id, target_room_id)),
    )
    .await
}
