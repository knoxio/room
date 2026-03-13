pub mod bridge;
pub mod queue;
pub mod stats;
pub mod taskboard;

use std::{collections::HashMap, path::Path};

// Re-export all plugin framework types from room-protocol so that existing
// imports from `crate::plugin::*` continue to work without changes.
pub use room_protocol::plugin::{
    BoxFuture, CommandContext, CommandInfo, HistoryAccess, MessageWriter, ParamSchema, ParamType,
    Plugin, PluginResult, RoomMetadata, UserInfo,
};

// Re-export concrete bridge types. ChatWriter and HistoryReader are public
// (used in tests and by broker/commands.rs). snapshot_metadata is crate-only.
pub(crate) use bridge::snapshot_metadata;
pub use bridge::{ChatWriter, HistoryReader};

// ── PluginRegistry ──────────────────────────────────────────────────────────

/// Built-in command names that plugins may not override.
const RESERVED_COMMANDS: &[&str] = &[
    "who",
    "help",
    "info",
    "kick",
    "reauth",
    "clear-tokens",
    "dm",
    "reply",
    "room-info",
    "exit",
    "clear",
    "subscribe",
    "set_subscription",
    "unsubscribe",
    "subscribe_events",
    "set_event_filter",
    "set_status",
    "subscriptions",
];

/// Central registry of plugins. The broker uses this to dispatch `/` commands.
pub struct PluginRegistry {
    plugins: Vec<Box<dyn Plugin>>,
    /// command_name → index into `plugins`.
    command_map: HashMap<String, usize>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            command_map: HashMap::new(),
        }
    }

    /// Create a registry with all standard plugins registered.
    ///
    /// Both standalone and daemon broker modes should call this so that every
    /// room has the same set of `/` commands available.
    pub(crate) fn with_all_plugins(chat_path: &Path) -> anyhow::Result<Self> {
        let mut registry = Self::new();

        let queue_path = queue::QueuePlugin::queue_path_from_chat(chat_path);
        registry.register(Box::new(queue::QueuePlugin::new(queue_path)?))?;

        registry.register(Box::new(stats::StatsPlugin))?;

        let taskboard_path = taskboard::TaskboardPlugin::taskboard_path_from_chat(chat_path);
        registry.register(Box::new(taskboard::TaskboardPlugin::new(
            taskboard_path,
            None,
        )))?;

        Ok(registry)
    }

    /// Register a plugin. Returns an error if any command name collides with
    /// a built-in command or another plugin's command.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) -> anyhow::Result<()> {
        let idx = self.plugins.len();
        for cmd in plugin.commands() {
            if RESERVED_COMMANDS.contains(&cmd.name.as_str()) {
                anyhow::bail!(
                    "plugin '{}' cannot register command '{}': reserved by built-in",
                    plugin.name(),
                    cmd.name
                );
            }
            if let Some(&existing_idx) = self.command_map.get(&cmd.name) {
                anyhow::bail!(
                    "plugin '{}' cannot register command '{}': already registered by '{}'",
                    plugin.name(),
                    cmd.name,
                    self.plugins[existing_idx].name()
                );
            }
            self.command_map.insert(cmd.name.clone(), idx);
        }
        self.plugins.push(plugin);
        Ok(())
    }

    /// Look up which plugin handles a command name.
    pub fn resolve(&self, command: &str) -> Option<&dyn Plugin> {
        self.command_map
            .get(command)
            .map(|&idx| self.plugins[idx].as_ref())
    }

    /// All registered commands across all plugins.
    pub fn all_commands(&self) -> Vec<CommandInfo> {
        self.plugins.iter().flat_map(|p| p.commands()).collect()
    }

    /// Notify all registered plugins that a user has joined the room.
    ///
    /// Calls [`Plugin::on_user_join`] on every plugin in registration order.
    pub fn notify_join(&self, user: &str) {
        for plugin in &self.plugins {
            plugin.on_user_join(user);
        }
    }

    /// Notify all registered plugins that a user has left the room.
    ///
    /// Calls [`Plugin::on_user_leave`] on every plugin in registration order.
    pub fn notify_leave(&self, user: &str) {
        for plugin in &self.plugins {
            plugin.on_user_leave(user);
        }
    }

    /// Completions for a specific command at a given argument position,
    /// derived from the parameter schema.
    ///
    /// Returns `Choice` values for `ParamType::Choice` parameters, or an
    /// empty vec for freeform/username/number parameters.
    pub fn completions_for(&self, command: &str, arg_pos: usize) -> Vec<String> {
        self.all_commands()
            .iter()
            .find(|c| c.name == command)
            .and_then(|c| c.params.get(arg_pos))
            .map(|p| match &p.param_type {
                ParamType::Choice(values) => values.clone(),
                _ => vec![],
            })
            .unwrap_or_default()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

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

    struct DummyPlugin {
        name: &'static str,
        cmd: &'static str,
    }

    impl Plugin for DummyPlugin {
        fn name(&self) -> &str {
            self.name
        }

        fn commands(&self) -> Vec<CommandInfo> {
            vec![CommandInfo {
                name: self.cmd.to_owned(),
                description: "dummy".to_owned(),
                usage: format!("/{}", self.cmd),
                params: vec![],
            }]
        }

        fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
            Box::pin(async { Ok(PluginResult::Reply("dummy".to_owned())) })
        }
    }

    #[test]
    fn registry_register_and_resolve() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(DummyPlugin {
            name: "test",
            cmd: "foo",
        }))
        .unwrap();
        assert!(reg.resolve("foo").is_some());
        assert!(reg.resolve("bar").is_none());
    }

    #[test]
    fn registry_rejects_reserved_command() {
        let mut reg = PluginRegistry::new();
        let result = reg.register(Box::new(DummyPlugin {
            name: "bad",
            cmd: "kick",
        }));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("reserved by built-in"));
    }

    #[test]
    fn registry_rejects_duplicate_command() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(DummyPlugin {
            name: "first",
            cmd: "foo",
        }))
        .unwrap();
        let result = reg.register(Box::new(DummyPlugin {
            name: "second",
            cmd: "foo",
        }));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("already registered by 'first'"));
    }

    #[test]
    fn registry_all_commands_lists_everything() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(DummyPlugin {
            name: "a",
            cmd: "alpha",
        }))
        .unwrap();
        reg.register(Box::new(DummyPlugin {
            name: "b",
            cmd: "beta",
        }))
        .unwrap();
        let cmds = reg.all_commands();
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn registry_completions_for_returns_choice_values() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new({
            struct CompPlugin;
            impl Plugin for CompPlugin {
                fn name(&self) -> &str {
                    "comp"
                }
                fn commands(&self) -> Vec<CommandInfo> {
                    vec![CommandInfo {
                        name: "test".to_owned(),
                        description: "test".to_owned(),
                        usage: "/test".to_owned(),
                        params: vec![ParamSchema {
                            name: "count".to_owned(),
                            param_type: ParamType::Choice(vec!["10".to_owned(), "20".to_owned()]),
                            required: false,
                            description: "Number of items".to_owned(),
                        }],
                    }]
                }
                fn handle(
                    &self,
                    _ctx: CommandContext,
                ) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
                    Box::pin(async { Ok(PluginResult::Handled) })
                }
            }
            CompPlugin
        }))
        .unwrap();
        let completions = reg.completions_for("test", 0);
        assert_eq!(completions, vec!["10", "20"]);
        assert!(reg.completions_for("test", 1).is_empty());
        assert!(reg.completions_for("nonexistent", 0).is_empty());
    }

    #[test]
    fn registry_completions_for_non_choice_returns_empty() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new({
            struct TextPlugin;
            impl Plugin for TextPlugin {
                fn name(&self) -> &str {
                    "text"
                }
                fn commands(&self) -> Vec<CommandInfo> {
                    vec![CommandInfo {
                        name: "echo".to_owned(),
                        description: "echo".to_owned(),
                        usage: "/echo".to_owned(),
                        params: vec![ParamSchema {
                            name: "msg".to_owned(),
                            param_type: ParamType::Text,
                            required: true,
                            description: "Message".to_owned(),
                        }],
                    }]
                }
                fn handle(
                    &self,
                    _ctx: CommandContext,
                ) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
                    Box::pin(async { Ok(PluginResult::Handled) })
                }
            }
            TextPlugin
        }))
        .unwrap();
        // Text params produce no completions
        assert!(reg.completions_for("echo", 0).is_empty());
    }

    #[test]
    fn registry_rejects_all_reserved_commands() {
        for &reserved in RESERVED_COMMANDS {
            let mut reg = PluginRegistry::new();
            let result = reg.register(Box::new(DummyPlugin {
                name: "bad",
                cmd: reserved,
            }));
            assert!(
                result.is_err(),
                "should reject reserved command '{reserved}'"
            );
        }
    }

    // ParamType tests moved to room-protocol — only room-cli-specific tests remain here.

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

    // HistoryReader tests live in bridge.rs alongside the implementation.

    // ── Plugin trait default methods ──────────────────────────────────────

    /// A plugin that only provides a name and handle — no commands override,
    /// no lifecycle hooks override. Demonstrates the defaults compile and work.
    struct MinimalPlugin;

    impl Plugin for MinimalPlugin {
        fn name(&self) -> &str {
            "minimal"
        }

        fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
            Box::pin(async { Ok(PluginResult::Handled) })
        }
        // commands(), on_user_join(), on_user_leave() all use defaults
    }

    #[test]
    fn default_commands_returns_empty_vec() {
        assert!(MinimalPlugin.commands().is_empty());
    }

    #[test]
    fn default_lifecycle_hooks_are_noop() {
        // These should not panic or do anything observable
        MinimalPlugin.on_user_join("alice");
        MinimalPlugin.on_user_leave("alice");
    }

    #[test]
    fn registry_notify_join_calls_all_plugins() {
        use std::sync::{Arc, Mutex};

        struct TrackingPlugin {
            joined: Arc<Mutex<Vec<String>>>,
            left: Arc<Mutex<Vec<String>>>,
        }

        impl Plugin for TrackingPlugin {
            fn name(&self) -> &str {
                "tracking"
            }

            fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
                Box::pin(async { Ok(PluginResult::Handled) })
            }

            fn on_user_join(&self, user: &str) {
                self.joined.lock().unwrap().push(user.to_owned());
            }

            fn on_user_leave(&self, user: &str) {
                self.left.lock().unwrap().push(user.to_owned());
            }
        }

        let joined = Arc::new(Mutex::new(Vec::<String>::new()));
        let left = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(TrackingPlugin {
            joined: joined.clone(),
            left: left.clone(),
        }))
        .unwrap();

        reg.notify_join("alice");
        reg.notify_join("bob");
        reg.notify_leave("alice");

        assert_eq!(*joined.lock().unwrap(), vec!["alice", "bob"]);
        assert_eq!(*left.lock().unwrap(), vec!["alice"]);
    }

    #[test]
    fn registry_notify_join_empty_registry_is_noop() {
        let reg = PluginRegistry::new();
        // Should not panic with zero plugins
        reg.notify_join("alice");
        reg.notify_leave("alice");
    }

    #[test]
    fn minimal_plugin_can_be_registered_without_commands() {
        let mut reg = PluginRegistry::new();
        // MinimalPlugin has no commands, so registration must succeed
        // (the only validation in register() is command name conflicts)
        reg.register(Box::new(MinimalPlugin)).unwrap();
        // It won't show up in resolve() since it has no commands
        assert_eq!(reg.all_commands().len(), 0);
    }
}
