pub mod queue;
pub mod stats;
pub mod taskboard;

use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use chrono::{DateTime, Utc};

use crate::{
    broker::{
        fanout::broadcast_and_persist,
        state::{ClientMap, StatusMap},
    },
    history,
    message::{make_event, make_system, EventType, Message},
};

/// Boxed future type used by [`Plugin::handle`] for dyn compatibility.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ── Plugin trait ────────────────────────────────────────────────────────────

/// A plugin that handles one or more `/` commands and/or reacts to room
/// lifecycle events.
///
/// Implement this trait and register it with [`PluginRegistry`] to add
/// custom commands to a room broker. The broker dispatches matching
/// `Message::Command` messages to the plugin's [`handle`](Plugin::handle)
/// method, and calls [`on_user_join`](Plugin::on_user_join) /
/// [`on_user_leave`](Plugin::on_user_leave) when users enter or leave.
///
/// Only [`name`](Plugin::name) and [`handle`](Plugin::handle) are required.
/// All other methods have no-op / empty-vec defaults so that adding new
/// lifecycle hooks in future releases does not break existing plugins.
pub trait Plugin: Send + Sync {
    /// Unique identifier for this plugin (e.g. `"stats"`, `"help"`).
    fn name(&self) -> &str;

    /// Commands this plugin handles. Each entry drives `/help` output
    /// and TUI autocomplete.
    ///
    /// Defaults to an empty vec for plugins that only use lifecycle hooks
    /// and do not register any commands.
    fn commands(&self) -> Vec<CommandInfo> {
        vec![]
    }

    /// Handle an invocation of one of this plugin's commands.
    ///
    /// Returns a boxed future for dyn compatibility (required because the
    /// registry stores `Box<dyn Plugin>`).
    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>>;

    /// Called after a user joins the room. The default is a no-op.
    ///
    /// Invoked synchronously during the join broadcast path. Implementations
    /// must not block — spawn a task if async work is needed.
    fn on_user_join(&self, _user: &str) {}

    /// Called after a user leaves the room. The default is a no-op.
    ///
    /// Invoked synchronously during the leave broadcast path. Implementations
    /// must not block — spawn a task if async work is needed.
    fn on_user_leave(&self, _user: &str) {}
}

// ── CommandInfo ─────────────────────────────────────────────────────────────

/// Describes a single command for `/help` and autocomplete.
#[derive(Debug, Clone)]
pub struct CommandInfo {
    /// Command name without the leading `/`.
    pub name: String,
    /// One-line description shown in `/help` and autocomplete.
    pub description: String,
    /// Usage string (e.g. `"/stats [last N]"`).
    pub usage: String,
    /// Typed parameter schemas for validation and autocomplete.
    pub params: Vec<ParamSchema>,
}

// ── Typed parameter schema ─────────────────────────────────────────────────

/// Schema for a single command parameter — drives validation, `/help` output,
/// and TUI argument autocomplete.
#[derive(Debug, Clone)]
pub struct ParamSchema {
    /// Display name (e.g. `"username"`, `"count"`).
    pub name: String,
    /// What kind of value this parameter accepts.
    pub param_type: ParamType,
    /// Whether the parameter must be provided.
    pub required: bool,
    /// One-line description shown in `/help <command>`.
    pub description: String,
}

/// The kind of value a parameter accepts.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamType {
    /// Free-form text (no validation beyond presence).
    Text,
    /// One of a fixed set of allowed values.
    Choice(Vec<String>),
    /// An online username — TUI shows the mention picker.
    Username,
    /// An integer, optionally bounded.
    Number { min: Option<i64>, max: Option<i64> },
}

// ── CommandContext ───────────────────────────────────────────────────────────

/// Context passed to a plugin's `handle` method.
pub struct CommandContext {
    /// The command name that was invoked (without `/`).
    pub command: String,
    /// Arguments passed after the command name.
    pub params: Vec<String>,
    /// Username of the invoker.
    pub sender: String,
    /// Room ID.
    pub room_id: String,
    /// Message ID that triggered this command.
    pub message_id: String,
    /// Timestamp of the triggering message.
    pub timestamp: DateTime<Utc>,
    /// Scoped handle for reading chat history.
    pub history: HistoryReader,
    /// Scoped handle for writing back to the chat.
    pub writer: ChatWriter,
    /// Snapshot of room metadata.
    pub metadata: RoomMetadata,
    /// All registered commands (so `/help` can list them without
    /// holding a reference to the registry).
    pub available_commands: Vec<CommandInfo>,
}

// ── PluginResult ────────────────────────────────────────────────────────────

/// What the broker should do after a plugin handles a command.
pub enum PluginResult {
    /// Send a private reply only to the invoker.
    Reply(String),
    /// Broadcast a message to the entire room.
    Broadcast(String),
    /// Command handled silently (side effects already done via [`ChatWriter`]).
    Handled,
}

// ── HistoryReader ───────────────────────────────────────────────────────────

/// Scoped read-only handle to a room's chat history.
///
/// Respects DM visibility — a plugin invoked by user X will not see DMs
/// between Y and Z.
pub struct HistoryReader {
    chat_path: PathBuf,
    viewer: String,
}

impl HistoryReader {
    pub(crate) fn new(chat_path: &Path, viewer: &str) -> Self {
        Self {
            chat_path: chat_path.to_owned(),
            viewer: viewer.to_owned(),
        }
    }

    /// Load all messages (filtered by DM visibility).
    pub async fn all(&self) -> anyhow::Result<Vec<Message>> {
        let all = history::load(&self.chat_path).await?;
        Ok(self.filter_dms(all))
    }

    /// Load the last `n` messages (filtered by DM visibility).
    pub async fn tail(&self, n: usize) -> anyhow::Result<Vec<Message>> {
        let all = history::tail(&self.chat_path, n).await?;
        Ok(self.filter_dms(all))
    }

    /// Load messages after the one with the given ID (filtered by DM visibility).
    pub async fn since(&self, message_id: &str) -> anyhow::Result<Vec<Message>> {
        let all = history::load(&self.chat_path).await?;
        let start = all
            .iter()
            .position(|m| m.id() == message_id)
            .map(|i| i + 1)
            .unwrap_or(0);
        Ok(self.filter_dms(all[start..].to_vec()))
    }

    /// Count total messages in the chat.
    pub async fn count(&self) -> anyhow::Result<usize> {
        let all = history::load(&self.chat_path).await?;
        Ok(all.len())
    }

    fn filter_dms(&self, messages: Vec<Message>) -> Vec<Message> {
        messages
            .into_iter()
            .filter(|m| match m {
                Message::DirectMessage { user, to, .. } => {
                    user == &self.viewer || to == &self.viewer
                }
                _ => true,
            })
            .collect()
    }
}

// ── ChatWriter ──────────────────────────────────────────────────────────────

/// Short-lived scoped handle for a plugin to write messages to the chat.
///
/// Posts as `plugin:<name>` — plugins cannot impersonate users. The writer
/// is valid only for the duration of [`Plugin::handle`].
pub struct ChatWriter {
    clients: ClientMap,
    chat_path: Arc<PathBuf>,
    room_id: Arc<String>,
    seq_counter: Arc<AtomicU64>,
    /// Identity the writer posts as (e.g. `"plugin:stats"`).
    identity: String,
}

impl ChatWriter {
    pub(crate) fn new(
        clients: &ClientMap,
        chat_path: &Arc<PathBuf>,
        room_id: &Arc<String>,
        seq_counter: &Arc<AtomicU64>,
        plugin_name: &str,
    ) -> Self {
        Self {
            clients: clients.clone(),
            chat_path: chat_path.clone(),
            room_id: room_id.clone(),
            seq_counter: seq_counter.clone(),
            identity: format!("plugin:{plugin_name}"),
        }
    }

    /// Broadcast a system message to all connected clients and persist to history.
    pub async fn broadcast(&self, content: &str) -> anyhow::Result<()> {
        let msg = make_system(&self.room_id, &self.identity, content);
        broadcast_and_persist(&msg, &self.clients, &self.chat_path, &self.seq_counter).await?;
        Ok(())
    }

    /// Send a private system message only to a specific user.
    pub async fn reply_to(&self, username: &str, content: &str) -> anyhow::Result<()> {
        let msg = make_system(&self.room_id, &self.identity, content);
        let seq = self.seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let mut msg = msg;
        msg.set_seq(seq);
        history::append(&self.chat_path, &msg).await?;

        let line = format!("{}\n", serde_json::to_string(&msg)?);
        let map = self.clients.lock().await;
        for (uname, tx) in map.values() {
            if uname == username {
                let _ = tx.send(line.clone());
            }
        }
        Ok(())
    }

    /// Broadcast a typed event to all connected clients and persist to history.
    pub async fn emit_event(
        &self,
        event_type: EventType,
        content: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let msg = make_event(&self.room_id, &self.identity, event_type, content, params);
        broadcast_and_persist(&msg, &self.clients, &self.chat_path, &self.seq_counter).await?;
        Ok(())
    }
}

// ── RoomMetadata ────────────────────────────────────────────────────────────

/// Frozen snapshot of room state for plugin consumption.
pub struct RoomMetadata {
    /// Users currently online with their status.
    pub online_users: Vec<UserInfo>,
    /// Username of the room host.
    pub host: Option<String>,
    /// Total messages in the chat file.
    pub message_count: usize,
}

/// A user's online presence.
pub struct UserInfo {
    pub username: String,
    pub status: String,
}

impl RoomMetadata {
    pub(crate) async fn snapshot(
        status_map: &StatusMap,
        host_user: &Arc<tokio::sync::Mutex<Option<String>>>,
        chat_path: &Path,
    ) -> Self {
        let map = status_map.lock().await;
        let online_users: Vec<UserInfo> = map
            .iter()
            .map(|(u, s)| UserInfo {
                username: u.clone(),
                status: s.clone(),
            })
            .collect();
        drop(map);

        let host = host_user.lock().await.clone();

        let message_count = history::load(chat_path)
            .await
            .map(|msgs| msgs.len())
            .unwrap_or(0);

        Self {
            online_users,
            host,
            message_count,
        }
    }
}

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

    // ── ParamSchema / ParamType tests ───────────────────────────────────────

    #[test]
    fn param_type_choice_equality() {
        let a = ParamType::Choice(vec!["x".to_owned(), "y".to_owned()]);
        let b = ParamType::Choice(vec!["x".to_owned(), "y".to_owned()]);
        assert_eq!(a, b);
        let c = ParamType::Choice(vec!["x".to_owned()]);
        assert_ne!(a, c);
    }

    #[test]
    fn param_type_number_equality() {
        let a = ParamType::Number {
            min: Some(1),
            max: Some(100),
        };
        let b = ParamType::Number {
            min: Some(1),
            max: Some(100),
        };
        assert_eq!(a, b);
        let c = ParamType::Number {
            min: None,
            max: None,
        };
        assert_ne!(a, c);
    }

    #[test]
    fn param_type_variants_are_distinct() {
        assert_ne!(ParamType::Text, ParamType::Username);
        assert_ne!(
            ParamType::Text,
            ParamType::Number {
                min: None,
                max: None
            }
        );
        assert_ne!(ParamType::Text, ParamType::Choice(vec!["a".to_owned()]));
    }

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

    #[tokio::test]
    async fn history_reader_filters_dms() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();

        // Write a DM between alice and bob, and a public message
        let dm = crate::message::make_dm("r", "alice", "bob", "secret");
        let public = crate::message::make_message("r", "carol", "hello all");
        history::append(path, &dm).await.unwrap();
        history::append(path, &public).await.unwrap();

        // alice sees both
        let reader_alice = HistoryReader::new(path, "alice");
        let msgs = reader_alice.all().await.unwrap();
        assert_eq!(msgs.len(), 2);

        // carol sees only the public message
        let reader_carol = HistoryReader::new(path, "carol");
        let msgs = reader_carol.all().await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].user(), "carol");
    }

    #[tokio::test]
    async fn history_reader_tail_and_count() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();

        for i in 0..5 {
            history::append(
                path,
                &crate::message::make_message("r", "u", format!("msg {i}")),
            )
            .await
            .unwrap();
        }

        let reader = HistoryReader::new(path, "u");
        assert_eq!(reader.count().await.unwrap(), 5);

        let tail = reader.tail(3).await.unwrap();
        assert_eq!(tail.len(), 3);
    }

    #[tokio::test]
    async fn history_reader_since() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();

        let msg1 = crate::message::make_message("r", "u", "first");
        let msg2 = crate::message::make_message("r", "u", "second");
        let msg3 = crate::message::make_message("r", "u", "third");
        let id1 = msg1.id().to_owned();
        history::append(path, &msg1).await.unwrap();
        history::append(path, &msg2).await.unwrap();
        history::append(path, &msg3).await.unwrap();

        let reader = HistoryReader::new(path, "u");
        let since = reader.since(&id1).await.unwrap();
        assert_eq!(since.len(), 2);
    }

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
