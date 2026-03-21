use room_protocol::make_system;

use crate::plugin::{builtin_command_infos, CommandInfo, ParamType};

use super::CommandResult;
use crate::broker::state::RoomState;

/// Handle `/info [username]` — dispatch to room info or user info.
pub(super) async fn handle_info_cmd(
    params: &[String],
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    let result = handle_info(params, state).await;
    let sys = make_system(&state.room_id, "broker", result);
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::Reply(json))
}

/// Handle `/help [command]` — list commands or show detail for one.
pub(super) fn handle_help_cmd(
    params: &[String],
    state: &RoomState,
) -> anyhow::Result<CommandResult> {
    let reply = handle_help(params, state);
    let sys = make_system(&state.room_id, "broker", reply);
    let json = serde_json::to_string(&sys)?;
    Ok(CommandResult::Reply(json))
}

/// Handle `/info [username]` — show room metadata or user info.
///
/// - `/info` (no args): room ID, visibility, config, host, member count, subscribers.
/// - `/info <username>`: status, subscription tier, host flag, online status.
///
/// `/room-info` is an alias that always shows room info (ignores params).
pub(super) async fn handle_info(params: &[String], state: &RoomState) -> String {
    if let Some(target) = params.first() {
        let target = target.strip_prefix('@').unwrap_or(target);
        return handle_user_info(target, state).await;
    }
    handle_room_info(state).await
}

/// Room metadata: visibility, config, host, member count, subscribers.
pub(super) async fn handle_room_info(state: &RoomState) -> String {
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
pub(super) async fn handle_user_info(target: &str, state: &RoomState) -> String {
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

/// Handle `/help [command] [subcommand]` — lists commands or shows detail.
pub(super) fn handle_help(params: &[String], state: &RoomState) -> String {
    let builtins = builtin_command_infos();
    let plugin_cmds = state.plugin_registry.all_commands();

    if let Some(target) = params.first() {
        let target = target.strip_prefix('/').unwrap_or(target);
        let sub_target = params.get(1).map(|s| s.as_str());

        // Check plugin commands first (matches original behaviour)
        if let Some(cmd) = plugin_cmds.iter().find(|c| c.name == target) {
            return resolve_help(cmd, sub_target);
        }
        // Then builtins
        if let Some(cmd) = builtins.iter().find(|c| c.name == target) {
            return resolve_help(cmd, sub_target);
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

/// Resolve help for a command, optionally drilling into a subcommand.
fn resolve_help(cmd: &CommandInfo, sub_target: Option<&str>) -> String {
    if let Some(sub) = sub_target {
        if !cmd.subcommands.is_empty() {
            if let Some(subcmd) = cmd.subcommands.iter().find(|s| s.name == sub) {
                return format_command_help(subcmd);
            }
            let names: Vec<&str> = cmd.subcommands.iter().map(|s| s.name.as_str()).collect();
            return format!(
                "unknown subcommand: /{} {sub}. available: {}",
                cmd.name,
                names.join(", ")
            );
        }
        // No subcommands defined — show the main command help
    }

    // If command has subcommands, list them alongside the main help
    if cmd.subcommands.is_empty() {
        format_command_help(cmd)
    } else {
        let mut lines = vec![format_command_help(cmd)];
        lines.push("  subcommands:".to_owned());
        for sub in &cmd.subcommands {
            lines.push(format!("    {} — {}", sub.name, sub.description));
        }
        lines.push(format!("  use /help {} <subcommand> for details", cmd.name));
        lines.join("\n")
    }
}

/// Format detailed help for a single command, including typed parameter info.
pub(super) fn format_command_help(cmd: &CommandInfo) -> String {
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
