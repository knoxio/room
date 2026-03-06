pub mod help;
pub mod stats;

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
    message::{make_system, Message},
};

/// Boxed future type used by [`Plugin::handle`] for dyn compatibility.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ── Plugin trait ────────────────────────────────────────────────────────────

/// A plugin that handles one or more `/` commands.
///
/// Implement this trait and register it with [`PluginRegistry`] to add
/// custom commands to a room broker. The broker dispatches matching
/// `Message::Command` messages to the plugin's [`handle`](Plugin::handle)
/// method.
pub trait Plugin: Send + Sync {
    /// Unique identifier for this plugin (e.g. `"stats"`, `"help"`).
    fn name(&self) -> &str;

    /// Commands this plugin handles. Each entry drives `/help` output
    /// and TUI autocomplete.
    fn commands(&self) -> Vec<CommandInfo>;

    /// Handle an invocation of one of this plugin's commands.
    ///
    /// Returns a boxed future for dyn compatibility (required because the
    /// registry stores `Box<dyn Plugin>`).
    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>>;
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
    /// Static argument completions for autocomplete.
    pub completions: Vec<Completion>,
}

/// A static autocomplete hint for a command argument.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Argument position (0-indexed).
    pub position: usize,
    /// Possible values. Empty means freeform.
    pub values: Vec<String>,
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
    "set_status",
    "who",
    "kick",
    "reauth",
    "clear-tokens",
    "exit",
    "clear",
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

    /// Static completions for a specific command at a given argument position.
    pub fn completions_for(&self, command: &str, arg_pos: usize) -> Vec<String> {
        self.all_commands()
            .iter()
            .find(|c| c.name == command)
            .map(|c| {
                c.completions
                    .iter()
                    .filter(|comp| comp.position == arg_pos)
                    .flat_map(|comp| comp.values.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
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
                completions: vec![],
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
    fn registry_completions_for_returns_values() {
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
                        completions: vec![Completion {
                            position: 0,
                            values: vec!["10".to_owned(), "20".to_owned()],
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
}
