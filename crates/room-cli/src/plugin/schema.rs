use room_protocol::plugin::{CommandInfo, ParamSchema, ParamType, Plugin};

use super::{queue, stats, taskboard};

// ── Built-in command schemas ───────────────────────────────────────────────

/// Returns [`CommandInfo`] schemas for all built-in commands (those handled
/// directly by the broker, not by plugins). Used by the TUI palette and
/// `/help` to show a complete command list with typed parameter metadata.
pub fn builtin_command_infos() -> Vec<CommandInfo> {
    vec![
        CommandInfo {
            name: "dm".to_owned(),
            description: "Send a private message".to_owned(),
            usage: "/dm <user> <message>".to_owned(),
            params: vec![
                ParamSchema {
                    name: "user".to_owned(),
                    param_type: ParamType::Username,
                    required: true,
                    description: "Recipient username".to_owned(),
                },
                ParamSchema {
                    name: "message".to_owned(),
                    param_type: ParamType::Text,
                    required: true,
                    description: "Message content".to_owned(),
                },
            ],
        },
        CommandInfo {
            name: "reply".to_owned(),
            description: "Reply to a message".to_owned(),
            usage: "/reply <id> <message>".to_owned(),
            params: vec![
                ParamSchema {
                    name: "id".to_owned(),
                    param_type: ParamType::Text,
                    required: true,
                    description: "Message ID to reply to".to_owned(),
                },
                ParamSchema {
                    name: "message".to_owned(),
                    param_type: ParamType::Text,
                    required: true,
                    description: "Reply content".to_owned(),
                },
            ],
        },
        CommandInfo {
            name: "who".to_owned(),
            description: "List users in the room".to_owned(),
            usage: "/who".to_owned(),
            params: vec![],
        },
        CommandInfo {
            name: "kick".to_owned(),
            description: "Kick a user from the room".to_owned(),
            usage: "/kick <user>".to_owned(),
            params: vec![ParamSchema {
                name: "user".to_owned(),
                param_type: ParamType::Username,
                required: true,
                description: "User to kick (host only)".to_owned(),
            }],
        },
        CommandInfo {
            name: "reauth".to_owned(),
            description: "Invalidate a user's token".to_owned(),
            usage: "/reauth <user>".to_owned(),
            params: vec![ParamSchema {
                name: "user".to_owned(),
                param_type: ParamType::Username,
                required: true,
                description: "User to reauth (host only)".to_owned(),
            }],
        },
        CommandInfo {
            name: "clear-tokens".to_owned(),
            description: "Revoke all session tokens".to_owned(),
            usage: "/clear-tokens".to_owned(),
            params: vec![],
        },
        CommandInfo {
            name: "exit".to_owned(),
            description: "Shut down the broker".to_owned(),
            usage: "/exit".to_owned(),
            params: vec![],
        },
        CommandInfo {
            name: "clear".to_owned(),
            description: "Clear the room history".to_owned(),
            usage: "/clear".to_owned(),
            params: vec![],
        },
        CommandInfo {
            name: "info".to_owned(),
            description: "Show room metadata or user info".to_owned(),
            usage: "/info [username]".to_owned(),
            params: vec![ParamSchema {
                name: "username".to_owned(),
                param_type: ParamType::Username,
                required: false,
                description: "User to inspect (omit for room info)".to_owned(),
            }],
        },
        CommandInfo {
            name: "room-info".to_owned(),
            description: "Alias for /info — show room visibility, config, and member count"
                .to_owned(),
            usage: "/room-info".to_owned(),
            params: vec![],
        },
        CommandInfo {
            name: "subscribe".to_owned(),
            description: "Subscribe to this room".to_owned(),
            usage: "/subscribe [tier]".to_owned(),
            params: vec![ParamSchema {
                name: "tier".to_owned(),
                param_type: ParamType::Choice(vec!["full".to_owned(), "mentions_only".to_owned()]),
                required: false,
                description: "Subscription tier (default: full)".to_owned(),
            }],
        },
        CommandInfo {
            name: "set_subscription".to_owned(),
            description: "Alias for /subscribe — set subscription tier for this room".to_owned(),
            usage: "/set_subscription [tier]".to_owned(),
            params: vec![ParamSchema {
                name: "tier".to_owned(),
                param_type: ParamType::Choice(vec!["full".to_owned(), "mentions_only".to_owned()]),
                required: false,
                description: "Subscription tier (default: full)".to_owned(),
            }],
        },
        CommandInfo {
            name: "unsubscribe".to_owned(),
            description: "Unsubscribe from this room".to_owned(),
            usage: "/unsubscribe".to_owned(),
            params: vec![],
        },
        CommandInfo {
            name: "subscribe_events".to_owned(),
            description: "Set event type filter for this room".to_owned(),
            usage: "/subscribe_events [filter]".to_owned(),
            params: vec![ParamSchema {
                name: "filter".to_owned(),
                param_type: ParamType::Text,
                required: false,
                description: "all, none, or comma-separated event types (default: all)".to_owned(),
            }],
        },
        CommandInfo {
            name: "set_event_filter".to_owned(),
            description: "Alias for /subscribe_events — set event type filter".to_owned(),
            usage: "/set_event_filter [filter]".to_owned(),
            params: vec![ParamSchema {
                name: "filter".to_owned(),
                param_type: ParamType::Text,
                required: false,
                description: "all, none, or comma-separated event types (default: all)".to_owned(),
            }],
        },
        CommandInfo {
            name: "set_status".to_owned(),
            description: "Set your presence status".to_owned(),
            usage: "/set_status <status>".to_owned(),
            params: vec![ParamSchema {
                name: "status".to_owned(),
                param_type: ParamType::Text,
                required: false,
                description: "Status text (omit to clear)".to_owned(),
            }],
        },
        CommandInfo {
            name: "subscriptions".to_owned(),
            description: "List subscription tiers and event filters for this room".to_owned(),
            usage: "/subscriptions".to_owned(),
            params: vec![],
        },
        CommandInfo {
            name: "help".to_owned(),
            description: "List available commands or get help for a specific command".to_owned(),
            usage: "/help [command]".to_owned(),
            params: vec![ParamSchema {
                name: "command".to_owned(),
                param_type: ParamType::Text,
                required: false,
                description: "Command name to get help for".to_owned(),
            }],
        },
    ]
}

/// Returns command schemas for all known commands: built-ins + default plugins.
///
/// Used by the TUI to build its command palette at startup without needing
/// access to the broker's `PluginRegistry`.
pub fn all_known_commands() -> Vec<CommandInfo> {
    let mut cmds = builtin_command_infos();
    cmds.extend(queue::QueuePlugin::default_commands());
    cmds.extend(stats::StatsPlugin.commands());
    cmds.extend(taskboard::TaskboardPlugin::default_commands());
    cmds
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── builtin_command_infos tests ───────────────────────────────────────

    #[test]
    fn builtin_command_infos_covers_all_expected_commands() {
        let cmds = builtin_command_infos();
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        for expected in &[
            "dm",
            "reply",
            "who",
            "help",
            "info",
            "kick",
            "reauth",
            "clear-tokens",
            "exit",
            "clear",
            "room-info",
            "set_status",
            "subscribe",
            "set_subscription",
            "unsubscribe",
            "subscribe_events",
            "set_event_filter",
            "subscriptions",
        ] {
            assert!(
                names.contains(expected),
                "missing built-in command: {expected}"
            );
        }
    }

    #[test]
    fn builtin_command_infos_dm_has_username_param() {
        let cmds = builtin_command_infos();
        let dm = cmds.iter().find(|c| c.name == "dm").unwrap();
        assert_eq!(dm.params.len(), 2);
        assert_eq!(dm.params[0].param_type, ParamType::Username);
        assert!(dm.params[0].required);
        assert_eq!(dm.params[1].param_type, ParamType::Text);
    }

    #[test]
    fn builtin_command_infos_kick_has_username_param() {
        let cmds = builtin_command_infos();
        let kick = cmds.iter().find(|c| c.name == "kick").unwrap();
        assert_eq!(kick.params.len(), 1);
        assert_eq!(kick.params[0].param_type, ParamType::Username);
        assert!(kick.params[0].required);
    }

    #[test]
    fn builtin_command_infos_who_has_no_params() {
        let cmds = builtin_command_infos();
        let who = cmds.iter().find(|c| c.name == "who").unwrap();
        assert!(who.params.is_empty());
    }

    // ── all_known_commands tests ──────────────────────────────────────────

    #[test]
    fn all_known_commands_includes_builtins_and_plugins() {
        let cmds = all_known_commands();
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        // Built-ins
        assert!(names.contains(&"dm"));
        assert!(names.contains(&"who"));
        assert!(names.contains(&"kick"));
        assert!(names.contains(&"help"));
        // Plugins
        assert!(names.contains(&"stats"));
    }

    #[test]
    fn all_known_commands_no_duplicates() {
        let cmds = all_known_commands();
        let mut names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        let before = names.len();
        names.sort();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate command names found");
    }
}
