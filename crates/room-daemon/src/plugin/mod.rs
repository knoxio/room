pub mod bridge;
pub mod queue;
pub mod schema;
pub mod stats;

/// Re-export the taskboard plugin from its own crate.
pub use room_plugin_taskboard as taskboard;

use std::{collections::HashMap, path::Path};

// Re-export all plugin framework types from room-protocol so that existing
// imports from `crate::plugin::*` continue to work without changes.
pub use room_protocol::plugin::{
    BoxFuture, CommandContext, CommandInfo, HistoryAccess, MessageWriter, ParamSchema, ParamType,
    Plugin, PluginResult, RoomMetadata, TeamAccess, UserInfo, PLUGIN_API_VERSION, PROTOCOL_VERSION,
};

// Re-export concrete bridge types. ChatWriter and HistoryReader are public
// (used in tests and by broker/commands.rs). snapshot_metadata is crate-only.
pub(crate) use bridge::snapshot_metadata;
pub use bridge::{ChatWriter, HistoryReader, TeamChecker};
pub use schema::{all_known_commands, builtin_command_infos};

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
    "team",
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

        // Derive agent plugin paths from the chat path.
        let agent_state_path = chat_path.with_extension("agents");
        let agent_log_dir = chat_path.parent().unwrap_or(chat_path).join("agent-logs");
        // All rooms run through the daemon — use the daemon socket.
        let agent_socket_path = crate::paths::effective_socket_path(None);
        registry.register(Box::new(room_plugin_agent::AgentPlugin::new(
            agent_state_path,
            agent_socket_path,
            agent_log_dir,
        )))?;

        Ok(registry)
    }

    /// Register a plugin. Returns an error if:
    /// - any command name collides with a built-in or another plugin's command
    /// - `api_version()` exceeds the current [`PLUGIN_API_VERSION`]
    /// - `min_protocol()` is newer than the running [`PROTOCOL_VERSION`]
    pub fn register(&mut self, plugin: Box<dyn Plugin>) -> anyhow::Result<()> {
        // ── Version compatibility checks ────────────────────────────────
        let api_v = plugin.api_version();
        if api_v > PLUGIN_API_VERSION {
            anyhow::bail!(
                "plugin '{}' requires api_version {api_v} but broker supports up to {PLUGIN_API_VERSION}",
                plugin.name(),
            );
        }

        let min_proto = plugin.min_protocol();
        if !semver_satisfies(PROTOCOL_VERSION, min_proto) {
            anyhow::bail!(
                "plugin '{}' requires room-protocol >= {min_proto} but broker has {PROTOCOL_VERSION}",
                plugin.name(),
            );
        }

        // ── Command name uniqueness checks ──────────────────────────────
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

    /// Notify all registered plugins that a message was broadcast.
    ///
    /// Calls [`Plugin::on_message`] on every plugin in registration order.
    pub fn notify_message(&self, msg: &room_protocol::Message) {
        for plugin in &self.plugins {
            plugin.on_message(msg);
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

/// Returns `true` if `running >= required` using semver major.minor.patch
/// comparison. Malformed versions are treated as `(0, 0, 0)`.
fn semver_satisfies(running: &str, required: &str) -> bool {
    let parse = |s: &str| -> (u64, u64, u64) {
        let mut parts = s.split('.');
        let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    };
    parse(running) >= parse(required)
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
            Box::pin(async { Ok(PluginResult::Reply("dummy".to_owned(), None)) })
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

    // Schema tests (builtin_command_infos, all_known_commands) live in schema.rs.
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
    fn registry_notify_message_calls_all_plugins() {
        use std::sync::{Arc, Mutex};

        struct MessageTracker {
            messages: Arc<Mutex<Vec<String>>>,
        }

        impl Plugin for MessageTracker {
            fn name(&self) -> &str {
                "msg-tracker"
            }

            fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
                Box::pin(async { Ok(PluginResult::Handled) })
            }

            fn on_message(&self, msg: &room_protocol::Message) {
                self.messages.lock().unwrap().push(msg.user().to_owned());
            }
        }

        let messages = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(MessageTracker {
            messages: messages.clone(),
        }))
        .unwrap();

        let msg = room_protocol::make_message("room", "alice", "hello");
        reg.notify_message(&msg);
        reg.notify_message(&room_protocol::make_message("room", "bob", "hi"));

        let recorded = messages.lock().unwrap();
        assert_eq!(*recorded, vec!["alice", "bob"]);
    }

    #[test]
    fn registry_notify_message_empty_registry_is_noop() {
        let reg = PluginRegistry::new();
        let msg = room_protocol::make_message("room", "alice", "hello");
        // Should not panic with zero plugins
        reg.notify_message(&msg);
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

    // ── Edge-case tests (#577) ───────────────────────────────────────────

    #[test]
    fn failed_register_does_not_pollute_registry() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(DummyPlugin {
            name: "good",
            cmd: "foo",
        }))
        .unwrap();

        // Attempt to register a plugin with a reserved command name — must fail.
        let result = reg.register(Box::new(DummyPlugin {
            name: "bad",
            cmd: "kick",
        }));
        assert!(result.is_err());

        // Original registration must be intact.
        assert!(
            reg.resolve("foo").is_some(),
            "pre-existing command must still resolve"
        );
        assert_eq!(reg.all_commands().len(), 1, "command count must not change");
        // The failed plugin must not appear in any form.
        assert!(
            reg.resolve("kick").is_none(),
            "failed command must not be resolvable"
        );
    }

    #[test]
    fn all_builtin_schemas_have_valid_fields() {
        let cmds = super::schema::builtin_command_infos();
        assert!(!cmds.is_empty(), "builtins must not be empty");
        for cmd in &cmds {
            assert!(!cmd.name.is_empty(), "name must not be empty");
            assert!(
                !cmd.description.is_empty(),
                "description must not be empty for /{}",
                cmd.name
            );
            assert!(
                !cmd.usage.is_empty(),
                "usage must not be empty for /{}",
                cmd.name
            );
            for param in &cmd.params {
                assert!(
                    !param.name.is_empty(),
                    "param name must not be empty in /{}",
                    cmd.name
                );
                assert!(
                    !param.description.is_empty(),
                    "param description must not be empty in /{} param '{}'",
                    cmd.name,
                    param.name
                );
            }
        }
    }

    #[test]
    fn duplicate_plugin_names_with_different_commands_succeed() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(DummyPlugin {
            name: "same-name",
            cmd: "alpha",
        }))
        .unwrap();
        // Same plugin name, different command — only command uniqueness is enforced.
        reg.register(Box::new(DummyPlugin {
            name: "same-name",
            cmd: "beta",
        }))
        .unwrap();
        assert!(reg.resolve("alpha").is_some());
        assert!(reg.resolve("beta").is_some());
        assert_eq!(reg.all_commands().len(), 2);
    }

    #[test]
    fn completions_for_number_param_returns_empty() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new({
            struct NumPlugin;
            impl Plugin for NumPlugin {
                fn name(&self) -> &str {
                    "num"
                }
                fn commands(&self) -> Vec<CommandInfo> {
                    vec![CommandInfo {
                        name: "repeat".to_owned(),
                        description: "repeat".to_owned(),
                        usage: "/repeat".to_owned(),
                        params: vec![ParamSchema {
                            name: "count".to_owned(),
                            param_type: ParamType::Number {
                                min: Some(1),
                                max: Some(100),
                            },
                            required: true,
                            description: "Number of repetitions".to_owned(),
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
            NumPlugin
        }))
        .unwrap();
        // Number params must not produce completions — only Choice does.
        assert!(reg.completions_for("repeat", 0).is_empty());
    }

    // ── semver_satisfies tests ──────────────────────────────────────────

    #[test]
    fn semver_satisfies_equal_versions() {
        assert!(super::semver_satisfies("3.1.0", "3.1.0"));
    }

    #[test]
    fn semver_satisfies_running_newer_major() {
        assert!(super::semver_satisfies("4.0.0", "3.1.0"));
    }

    #[test]
    fn semver_satisfies_running_newer_minor() {
        assert!(super::semver_satisfies("3.2.0", "3.1.0"));
    }

    #[test]
    fn semver_satisfies_running_newer_patch() {
        assert!(super::semver_satisfies("3.1.1", "3.1.0"));
    }

    #[test]
    fn semver_satisfies_running_older_fails() {
        assert!(!super::semver_satisfies("3.0.9", "3.1.0"));
    }

    #[test]
    fn semver_satisfies_running_older_major_fails() {
        assert!(!super::semver_satisfies("2.9.9", "3.0.0"));
    }

    #[test]
    fn semver_satisfies_zero_required_always_passes() {
        assert!(super::semver_satisfies("0.0.1", "0.0.0"));
        assert!(super::semver_satisfies("3.1.0", "0.0.0"));
    }

    #[test]
    fn semver_satisfies_malformed_treated_as_zero() {
        assert!(super::semver_satisfies("garbage", "0.0.0"));
        assert!(super::semver_satisfies("3.1.0", "garbage"));
        assert!(super::semver_satisfies("garbage", "garbage"));
    }

    // ── Version compatibility in register() ─────────────────────────────

    /// A plugin that reports a future api_version the broker does not support.
    struct FutureApiPlugin;

    impl Plugin for FutureApiPlugin {
        fn name(&self) -> &str {
            "future-api"
        }

        fn api_version(&self) -> u32 {
            PLUGIN_API_VERSION + 1
        }

        fn commands(&self) -> Vec<CommandInfo> {
            vec![CommandInfo {
                name: "future".to_owned(),
                description: "from the future".to_owned(),
                usage: "/future".to_owned(),
                params: vec![],
            }]
        }

        fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
            Box::pin(async { Ok(PluginResult::Handled) })
        }
    }

    #[test]
    fn register_rejects_future_api_version() {
        let mut reg = PluginRegistry::new();
        let result = reg.register(Box::new(FutureApiPlugin));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("api_version"),
            "error should mention api_version: {err}"
        );
        assert!(
            err.contains("future-api"),
            "error should mention plugin name: {err}"
        );
    }

    /// A plugin that requires a protocol version newer than what we have.
    struct FutureProtocolPlugin;

    impl Plugin for FutureProtocolPlugin {
        fn name(&self) -> &str {
            "future-proto"
        }

        fn min_protocol(&self) -> &str {
            "99.0.0"
        }

        fn commands(&self) -> Vec<CommandInfo> {
            vec![CommandInfo {
                name: "proto".to_owned(),
                description: "needs future protocol".to_owned(),
                usage: "/proto".to_owned(),
                params: vec![],
            }]
        }

        fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
            Box::pin(async { Ok(PluginResult::Handled) })
        }
    }

    #[test]
    fn register_rejects_incompatible_min_protocol() {
        let mut reg = PluginRegistry::new();
        let result = reg.register(Box::new(FutureProtocolPlugin));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("room-protocol"),
            "error should mention room-protocol: {err}"
        );
        assert!(
            err.contains("99.0.0"),
            "error should mention required version: {err}"
        );
    }

    #[test]
    fn register_accepts_compatible_versioned_plugin() {
        let mut reg = PluginRegistry::new();
        // DummyPlugin uses defaults: api_version=1, min_protocol="0.0.0"
        let result = reg.register(Box::new(DummyPlugin {
            name: "compat",
            cmd: "compat_cmd",
        }));
        assert!(result.is_ok());
        assert!(reg.resolve("compat_cmd").is_some());
    }

    #[test]
    fn register_version_check_runs_before_command_check() {
        // A plugin with a future api_version AND a reserved command name.
        // The api_version check should fire first.
        struct DoubleBadPlugin;

        impl Plugin for DoubleBadPlugin {
            fn name(&self) -> &str {
                "double-bad"
            }

            fn api_version(&self) -> u32 {
                PLUGIN_API_VERSION + 1
            }

            fn commands(&self) -> Vec<CommandInfo> {
                vec![CommandInfo {
                    name: "kick".to_owned(),
                    description: "bad".to_owned(),
                    usage: "/kick".to_owned(),
                    params: vec![],
                }]
            }

            fn handle(&self, _ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
                Box::pin(async { Ok(PluginResult::Handled) })
            }
        }

        let mut reg = PluginRegistry::new();
        let result = reg.register(Box::new(DoubleBadPlugin));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // Should fail on api_version, not on the reserved command
        assert!(
            err.contains("api_version"),
            "should reject on api_version first: {err}"
        );
    }

    #[test]
    fn failed_version_check_does_not_pollute_registry() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(DummyPlugin {
            name: "good",
            cmd: "foo",
        }))
        .unwrap();

        // Attempt to register a plugin with incompatible protocol
        let result = reg.register(Box::new(FutureProtocolPlugin));
        assert!(result.is_err());

        // Original registration must be intact
        assert!(reg.resolve("foo").is_some());
        assert_eq!(reg.all_commands().len(), 1);
        // Failed plugin's command must not appear
        assert!(reg.resolve("proto").is_none());
    }
}
