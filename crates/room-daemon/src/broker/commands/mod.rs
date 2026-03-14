mod handlers;
pub(crate) mod info;
mod plugin;
mod team;
#[cfg(test)]
mod tests;
pub(crate) mod validate;

use room_protocol::{make_system, Message};

use crate::plugin::builtin_command_infos;

use super::{
    admin::{handle_admin_cmd, ADMIN_CMD_NAMES},
    state::RoomState,
};

use handlers::{
    handle_set_status, handle_subscribe, handle_subscribe_events, handle_subscriptions,
    handle_unsubscribe, handle_who, handle_who_all,
};
use info::{handle_help_cmd, handle_info_cmd};
use plugin::dispatch_plugin;
use team::handle_team;
use validate::validate_params;

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
            "who_all" => return handle_who_all(state).await,
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
            "team" => return handle_team(params, username, state).await,
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
