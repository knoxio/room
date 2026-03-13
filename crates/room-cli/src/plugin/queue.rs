use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{BoxFuture, CommandContext, CommandInfo, ParamSchema, ParamType, Plugin, PluginResult};

/// A single item in the task queue backlog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    /// Unique identifier for this queue entry.
    pub id: String,
    /// Human-readable task description.
    pub description: String,
    /// Username of the agent who added this item.
    pub added_by: String,
    /// Timestamp when the item was added.
    pub added_at: DateTime<Utc>,
    /// Current status — always `"queued"` while in the backlog.
    pub status: String,
}

/// Plugin that manages a persistent task backlog.
///
/// Agents can add tasks to the queue and self-assign from it using `/queue pop`.
///
/// Queue state is persisted to an NDJSON file alongside the room's `.chat` file.
pub struct QueuePlugin {
    /// In-memory queue items, protected by a mutex for concurrent access.
    queue: Arc<Mutex<Vec<QueueItem>>>,
    /// Path to the NDJSON persistence file (`<room-data-dir>/<room-id>.queue`).
    queue_path: PathBuf,
}

impl QueuePlugin {
    /// Create a new `QueuePlugin`, loading any existing queue from disk.
    ///
    /// # Arguments
    /// * `queue_path` — path to the `.queue` NDJSON file
    pub(crate) fn new(queue_path: PathBuf) -> anyhow::Result<Self> {
        let items = load_queue(&queue_path)?;
        Ok(Self {
            queue: Arc::new(Mutex::new(items)),
            queue_path,
        })
    }

    /// Derive the `.queue` file path from a `.chat` file path.
    pub fn queue_path_from_chat(chat_path: &Path) -> PathBuf {
        chat_path.with_extension("queue")
    }

    /// Returns the command info for the TUI command palette without needing
    /// an instantiated plugin. Used by `all_known_commands()`.
    pub fn default_commands() -> Vec<CommandInfo> {
        vec![CommandInfo {
            name: "queue".to_owned(),
            description: "Manage the task backlog".to_owned(),
            usage: "/queue <add|list|remove|pop> [args]".to_owned(),
            params: vec![
                ParamSchema {
                    name: "action".to_owned(),
                    param_type: ParamType::Choice(vec![
                        "add".to_owned(),
                        "list".to_owned(),
                        "remove".to_owned(),
                        "pop".to_owned(),
                    ]),
                    required: true,
                    description: "Action to perform".to_owned(),
                },
                ParamSchema {
                    name: "args".to_owned(),
                    param_type: ParamType::Text,
                    required: false,
                    description: "Task description (add) or index (remove)".to_owned(),
                },
            ],
        }]
    }
}

impl Plugin for QueuePlugin {
    fn name(&self) -> &str {
        "queue"
    }

    fn commands(&self) -> Vec<CommandInfo> {
        Self::default_commands()
    }

    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
        Box::pin(async move {
            let action = ctx.params.first().map(String::as_str).unwrap_or("");
            let rest: Vec<&str> = ctx.params.iter().skip(1).map(String::as_str).collect();

            match action {
                "add" => self.handle_add(&ctx.sender, &rest).await,
                "list" => self.handle_list(&ctx).await,
                "remove" => self.handle_remove(&rest).await,
                "pop" => self.handle_pop(&ctx.sender).await,
                _ => Ok(PluginResult::Reply(format!(
                    "queue: unknown action '{action}'. use add, list, remove, or pop"
                ))),
            }
        })
    }
}

impl QueuePlugin {
    async fn handle_add(&self, sender: &str, rest: &[&str]) -> anyhow::Result<PluginResult> {
        if rest.is_empty() {
            return Ok(PluginResult::Reply(
                "queue add: missing task description".to_owned(),
            ));
        }

        let description = rest.join(" ");
        let item = QueueItem {
            id: Uuid::new_v4().to_string(),
            description: description.clone(),
            added_by: sender.to_owned(),
            added_at: Utc::now(),
            status: "queued".to_owned(),
        };

        {
            let mut queue = self.queue.lock().await;
            queue.push(item.clone());
        }

        append_item(&self.queue_path, &item)?;
        let count = self.queue.lock().await.len();

        Ok(PluginResult::Broadcast(format!(
            "queue: {sender} added \"{description}\" (#{count} in backlog)"
        )))
    }

    async fn handle_list(&self, _ctx: &CommandContext) -> anyhow::Result<PluginResult> {
        let queue = self.queue.lock().await;

        if queue.is_empty() {
            return Ok(PluginResult::Reply("queue: backlog is empty".to_owned()));
        }

        let mut lines = Vec::with_capacity(queue.len() + 1);
        lines.push(format!("queue: {} item(s) in backlog", queue.len()));
        for (i, item) in queue.iter().enumerate() {
            lines.push(format!(
                "  {}. {} (added by {} at {})",
                i + 1,
                item.description,
                item.added_by,
                item.added_at.format("%Y-%m-%d %H:%M UTC")
            ));
        }

        Ok(PluginResult::Reply(lines.join("\n")))
    }

    async fn handle_remove(&self, rest: &[&str]) -> anyhow::Result<PluginResult> {
        let index_str = rest.first().copied().unwrap_or("");
        let index: usize = match index_str.parse::<usize>() {
            Ok(n) if n >= 1 => n,
            _ => {
                return Ok(PluginResult::Reply(
                    "queue remove: provide a valid 1-based index".to_owned(),
                ));
            }
        };

        let removed = {
            let mut queue = self.queue.lock().await;
            if index > queue.len() {
                return Ok(PluginResult::Reply(format!(
                    "queue remove: index {index} out of range (queue has {} item(s))",
                    queue.len()
                )));
            }
            queue.remove(index - 1)
        };

        self.rewrite_queue().await?;

        Ok(PluginResult::Broadcast(format!(
            "queue: removed \"{}\" (was #{index})",
            removed.description
        )))
    }

    async fn handle_pop(&self, sender: &str) -> anyhow::Result<PluginResult> {
        let popped = {
            let mut queue = self.queue.lock().await;
            if queue.is_empty() {
                return Ok(PluginResult::Reply(
                    "queue pop: backlog is empty, nothing to pop".to_owned(),
                ));
            }
            queue.remove(0)
        };

        self.rewrite_queue().await?;

        Ok(PluginResult::Broadcast(format!(
            "{sender} popped from queue: \"{}\"",
            popped.description
        )))
    }

    /// Rewrite the entire queue file from the in-memory state.
    async fn rewrite_queue(&self) -> anyhow::Result<()> {
        let queue = self.queue.lock().await;
        rewrite_queue_file(&self.queue_path, &queue)
    }
}

// ── Persistence helpers ─────────────────────────────────────────────────────

/// Load queue items from an NDJSON file. Returns an empty vec if the file
/// does not exist.
fn load_queue(path: &Path) -> anyhow::Result<Vec<QueueItem>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut items = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<QueueItem>(trimmed) {
            Ok(item) => items.push(item),
            Err(e) => {
                eprintln!("queue: skipping malformed line in {}: {e}", path.display());
            }
        }
    }

    Ok(items)
}

/// Append a single item to the NDJSON queue file.
fn append_item(path: &Path, item: &QueueItem) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(item)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Rewrite the entire queue file from a slice of items.
fn rewrite_queue_file(path: &Path, items: &[QueueItem]) -> anyhow::Result<()> {
    let mut file = std::fs::File::create(path)?;
    for item in items {
        let line = serde_json::to_string(item)?;
        writeln!(file, "{line}")?;
    }
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{ChatWriter, HistoryReader, RoomMetadata, UserInfo};
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    /// Helper: create a QueuePlugin with a temp directory.
    fn make_test_plugin(dir: &TempDir) -> QueuePlugin {
        let queue_path = dir.path().join("test-room.queue");
        QueuePlugin::new(queue_path).unwrap()
    }

    /// Helper: create a CommandContext wired to a test plugin.
    fn make_test_ctx(
        command: &str,
        params: Vec<&str>,
        sender: &str,
        chat_path: &Path,
        clients: &Arc<Mutex<HashMap<u64, (String, broadcast::Sender<String>)>>>,
        seq: &Arc<AtomicU64>,
    ) -> CommandContext {
        let chat_arc = Arc::new(chat_path.to_path_buf());
        let room_id = Arc::new("test-room".to_owned());

        CommandContext {
            command: command.to_owned(),
            params: params.into_iter().map(|s| s.to_owned()).collect(),
            sender: sender.to_owned(),
            room_id: "test-room".to_owned(),
            message_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            history: Box::new(HistoryReader::new(chat_path, sender)),
            writer: Box::new(ChatWriter::new(clients, &chat_arc, &room_id, seq, "queue")),
            metadata: RoomMetadata {
                online_users: vec![UserInfo {
                    username: sender.to_owned(),
                    status: String::new(),
                }],
                host: None,
                message_count: 0,
            },
            available_commands: vec![],
        }
    }

    /// Helper: set up broadcast channel and client map for tests.
    fn make_test_clients() -> (
        Arc<Mutex<HashMap<u64, (String, broadcast::Sender<String>)>>>,
        broadcast::Receiver<String>,
        Arc<AtomicU64>,
    ) {
        let (tx, rx) = broadcast::channel::<String>(64);
        let mut map = HashMap::new();
        map.insert(1u64, ("alice".to_owned(), tx));
        let clients = Arc::new(Mutex::new(map));
        let seq = Arc::new(AtomicU64::new(0));
        (clients, rx, seq)
    }

    // ── load/save persistence tests ─────────────────────────────────────────

    #[test]
    fn load_queue_nonexistent_file_returns_empty() {
        let items = load_queue(Path::new("/nonexistent/path.queue")).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn append_and_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.queue");

        let item = QueueItem {
            id: "id-1".to_owned(),
            description: "fix the bug".to_owned(),
            added_by: "alice".to_owned(),
            added_at: Utc::now(),
            status: "queued".to_owned(),
        };

        append_item(&path, &item).unwrap();
        let loaded = load_queue(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "id-1");
        assert_eq!(loaded[0].description, "fix the bug");
        assert_eq!(loaded[0].added_by, "alice");
        assert_eq!(loaded[0].status, "queued");
    }

    #[test]
    fn rewrite_replaces_file_contents() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.queue");

        // Write 3 items
        let items: Vec<QueueItem> = (0..3)
            .map(|i| QueueItem {
                id: format!("id-{i}"),
                description: format!("task {i}"),
                added_by: "bob".to_owned(),
                added_at: Utc::now(),
                status: "queued".to_owned(),
            })
            .collect();

        for item in &items {
            append_item(&path, item).unwrap();
        }
        assert_eq!(load_queue(&path).unwrap().len(), 3);

        // Rewrite with only 1 item
        rewrite_queue_file(&path, &items[1..2]).unwrap();
        let reloaded = load_queue(&path).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].id, "id-1");
    }

    #[test]
    fn load_skips_malformed_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.queue");

        let good = QueueItem {
            id: "good".to_owned(),
            description: "valid".to_owned(),
            added_by: "alice".to_owned(),
            added_at: Utc::now(),
            status: "queued".to_owned(),
        };

        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&good).unwrap()).unwrap();
        writeln!(file, "not valid json").unwrap();
        writeln!(file, "{}", serde_json::to_string(&good).unwrap()).unwrap();
        writeln!(file).unwrap(); // blank line

        let loaded = load_queue(&path).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn queue_path_from_chat_replaces_extension() {
        let chat = PathBuf::from("/data/room-dev.chat");
        let queue = QueuePlugin::queue_path_from_chat(&chat);
        assert_eq!(queue, PathBuf::from("/data/room-dev.queue"));
    }

    // ── plugin construction tests ───────────────────────────────────────────

    #[test]
    fn new_plugin_loads_existing_queue() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.queue");

        let item = QueueItem {
            id: "pre-existing".to_owned(),
            description: "already there".to_owned(),
            added_by: "ba".to_owned(),
            added_at: Utc::now(),
            status: "queued".to_owned(),
        };
        append_item(&path, &item).unwrap();

        let plugin = QueuePlugin::new(path).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let queue = plugin.queue.lock().await;
            assert_eq!(queue.len(), 1);
            assert_eq!(queue[0].description, "already there");
        });
    }

    // ── handle tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn add_appends_to_queue_and_persists() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();
        let ctx = make_test_ctx(
            "queue",
            vec!["add", "fix", "the", "bug"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );

        let result = plugin.handle(ctx).await.unwrap();
        match &result {
            PluginResult::Broadcast(msg) => {
                assert!(msg.contains("fix the bug"), "broadcast should mention task");
                assert!(msg.contains("alice"), "broadcast should mention sender");
                assert!(msg.contains("#1"), "broadcast should show queue position");
            }
            _ => panic!("expected Broadcast"),
        }

        // In-memory
        let queue = plugin.queue.lock().await;
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].description, "fix the bug");
        assert_eq!(queue[0].added_by, "alice");
        drop(queue);

        // On disk
        let persisted = load_queue(&dir.path().join("test-room.queue")).unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].description, "fix the bug");
    }

    #[tokio::test]
    async fn add_without_description_returns_error() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();
        let ctx = make_test_ctx(
            "queue",
            vec!["add"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );

        let result = plugin.handle(ctx).await.unwrap();
        match result {
            PluginResult::Reply(msg) => assert!(msg.contains("missing task description")),
            _ => panic!("expected Reply"),
        }
    }

    #[tokio::test]
    async fn list_empty_queue() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();
        let ctx = make_test_ctx(
            "queue",
            vec!["list"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );

        let result = plugin.handle(ctx).await.unwrap();
        match result {
            PluginResult::Reply(msg) => assert!(msg.contains("empty")),
            _ => panic!("expected Reply"),
        }
    }

    #[tokio::test]
    async fn list_shows_indexed_items() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();

        // Add two items
        for desc in ["first task", "second task"] {
            let words: Vec<&str> = std::iter::once("add")
                .chain(desc.split_whitespace())
                .collect();
            let ctx = make_test_ctx("queue", words, "alice", chat_tmp.path(), &clients, &seq);
            plugin.handle(ctx).await.unwrap();
        }

        let ctx = make_test_ctx(
            "queue",
            vec!["list"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        let result = plugin.handle(ctx).await.unwrap();
        match result {
            PluginResult::Reply(msg) => {
                assert!(msg.contains("2 item(s)"));
                assert!(msg.contains("1. first task"));
                assert!(msg.contains("2. second task"));
            }
            _ => panic!("expected Reply"),
        }
    }

    #[tokio::test]
    async fn remove_by_index() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();

        // Add two items
        for desc in ["first", "second"] {
            let ctx = make_test_ctx(
                "queue",
                vec!["add", desc],
                "alice",
                chat_tmp.path(),
                &clients,
                &seq,
            );
            plugin.handle(ctx).await.unwrap();
        }

        // Remove item 1
        let ctx = make_test_ctx(
            "queue",
            vec!["remove", "1"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        let result = plugin.handle(ctx).await.unwrap();
        match &result {
            PluginResult::Broadcast(msg) => {
                assert!(
                    msg.contains("first"),
                    "broadcast should mention removed item"
                );
                assert!(msg.contains("#1"), "broadcast should mention index");
            }
            _ => panic!("expected Broadcast"),
        }

        // Verify only "second" remains
        let queue = plugin.queue.lock().await;
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].description, "second");
        drop(queue);

        // Verify persistence
        let persisted = load_queue(&dir.path().join("test-room.queue")).unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].description, "second");
    }

    #[tokio::test]
    async fn remove_out_of_range() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();

        let ctx = make_test_ctx(
            "queue",
            vec!["remove", "5"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        let result = plugin.handle(ctx).await.unwrap();
        match result {
            PluginResult::Reply(msg) => assert!(msg.contains("out of range")),
            _ => panic!("expected Reply"),
        }
    }

    #[tokio::test]
    async fn remove_invalid_index() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();

        let ctx = make_test_ctx(
            "queue",
            vec!["remove", "abc"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        let result = plugin.handle(ctx).await.unwrap();
        match result {
            PluginResult::Reply(msg) => assert!(msg.contains("valid 1-based index")),
            _ => panic!("expected Reply"),
        }
    }

    #[tokio::test]
    async fn remove_zero_index() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();

        let ctx = make_test_ctx(
            "queue",
            vec!["remove", "0"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        let result = plugin.handle(ctx).await.unwrap();
        match result {
            PluginResult::Reply(msg) => assert!(msg.contains("valid 1-based index")),
            _ => panic!("expected Reply"),
        }
    }

    #[tokio::test]
    async fn pop_removes_first_item() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);

        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();

        // Add two items
        for desc in ["urgent fix", "nice to have"] {
            let words: Vec<&str> = std::iter::once("add")
                .chain(desc.split_whitespace())
                .collect();
            let ctx = make_test_ctx("queue", words, "ba", chat_tmp.path(), &clients, &seq);
            plugin.handle(ctx).await.unwrap();
        }

        // Pop as alice
        let ctx = make_test_ctx(
            "queue",
            vec!["pop"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        let result = plugin.handle(ctx).await.unwrap();
        match &result {
            PluginResult::Broadcast(msg) => {
                assert!(msg.contains("alice"), "broadcast should mention popper");
                assert!(msg.contains("urgent fix"), "broadcast should mention task");
            }
            _ => panic!("expected Broadcast"),
        }

        // Verify queue has only 1 item left
        let queue = plugin.queue.lock().await;
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].description, "nice to have");
        drop(queue);

        // Verify persistence
        let persisted = load_queue(&dir.path().join("test-room.queue")).unwrap();
        assert_eq!(persisted.len(), 1);
    }

    #[tokio::test]
    async fn pop_empty_queue_returns_error() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();

        let ctx = make_test_ctx(
            "queue",
            vec!["pop"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        let result = plugin.handle(ctx).await.unwrap();
        match result {
            PluginResult::Reply(msg) => assert!(msg.contains("empty")),
            _ => panic!("expected Reply"),
        }
    }

    #[tokio::test]
    async fn pop_fifo_order() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();

        // Add two items
        let ctx = make_test_ctx(
            "queue",
            vec!["add", "first"],
            "ba",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        plugin.handle(ctx).await.unwrap();
        let ctx = make_test_ctx(
            "queue",
            vec!["add", "second"],
            "ba",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        plugin.handle(ctx).await.unwrap();

        // Pop should return first item
        let ctx = make_test_ctx(
            "queue",
            vec!["pop"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        let result = plugin.handle(ctx).await.unwrap();
        match &result {
            PluginResult::Broadcast(msg) => assert!(msg.contains("first")),
            _ => panic!("expected Broadcast"),
        }

        // Pop again should return second item
        let ctx = make_test_ctx("queue", vec!["pop"], "bob", chat_tmp.path(), &clients, &seq);
        let result = plugin.handle(ctx).await.unwrap();
        match &result {
            PluginResult::Broadcast(msg) => assert!(msg.contains("second")),
            _ => panic!("expected Broadcast"),
        }

        // Queue is now empty
        assert!(plugin.queue.lock().await.is_empty());
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let chat_tmp = tempfile::NamedTempFile::new().unwrap();
        let (clients, _rx, seq) = make_test_clients();

        let ctx = make_test_ctx(
            "queue",
            vec!["foobar"],
            "alice",
            chat_tmp.path(),
            &clients,
            &seq,
        );
        let result = plugin.handle(ctx).await.unwrap();
        match result {
            PluginResult::Reply(msg) => assert!(msg.contains("unknown action")),
            _ => panic!("expected Reply"),
        }
    }

    #[tokio::test]
    async fn queue_survives_reload() {
        let dir = TempDir::new().unwrap();
        let queue_path = dir.path().join("test-room.queue");

        // First plugin instance — add items
        {
            let plugin = QueuePlugin::new(queue_path.clone()).unwrap();
            let chat_tmp = tempfile::NamedTempFile::new().unwrap();
            let (clients, _rx, seq) = make_test_clients();

            for desc in ["task a", "task b", "task c"] {
                let words: Vec<&str> = std::iter::once("add")
                    .chain(desc.split_whitespace())
                    .collect();
                let ctx = make_test_ctx("queue", words, "alice", chat_tmp.path(), &clients, &seq);
                plugin.handle(ctx).await.unwrap();
            }
        }

        // Second plugin instance — simulates broker restart
        let plugin2 = QueuePlugin::new(queue_path).unwrap();
        let queue = plugin2.queue.lock().await;
        assert_eq!(queue.len(), 3);
        assert_eq!(queue[0].description, "task a");
        assert_eq!(queue[1].description, "task b");
        assert_eq!(queue[2].description, "task c");
    }

    // ── Plugin trait tests ──────────────────────────────────────────────────

    #[test]
    fn plugin_name_is_queue() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        assert_eq!(plugin.name(), "queue");
    }

    #[test]
    fn plugin_registers_queue_command() {
        let dir = TempDir::new().unwrap();
        let plugin = make_test_plugin(&dir);
        let cmds = plugin.commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "queue");
        assert_eq!(cmds[0].params.len(), 2);
        assert!(cmds[0].params[0].required);
        assert!(!cmds[0].params[1].required);
    }
}
