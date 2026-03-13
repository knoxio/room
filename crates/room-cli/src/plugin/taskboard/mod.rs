mod handlers;
pub mod task;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use task::{next_id, LiveTask, Task, TaskStatus};

use room_protocol::EventType;

use crate::plugin::{
    BoxFuture, CommandContext, CommandInfo, ParamSchema, ParamType, Plugin, PluginResult,
};

/// Default lease TTL in seconds (10 minutes).
const DEFAULT_LEASE_TTL_SECS: u64 = 600;

/// Unified task lifecycle plugin with lease-based expiry.
///
/// Manages a board of tasks that agents can post, claim, plan, get approved,
/// update, release, and finish. Claimed tasks have a configurable lease TTL —
/// if not renewed via `/taskboard update` or `/taskboard plan`, they auto-
/// release back to open status (lazy sweep on access).
pub struct TaskboardPlugin {
    /// In-memory task board with lease timers.
    board: Arc<Mutex<Vec<LiveTask>>>,
    /// Path to the NDJSON persistence file.
    storage_path: PathBuf,
    /// Lease TTL duration.
    lease_ttl: Duration,
}

impl TaskboardPlugin {
    /// Create a new taskboard plugin, loading existing tasks from disk.
    pub fn new(storage_path: PathBuf, lease_ttl_secs: Option<u64>) -> Self {
        let ttl = lease_ttl_secs.unwrap_or(DEFAULT_LEASE_TTL_SECS);
        let tasks = task::load_tasks(&storage_path);
        let live_tasks: Vec<LiveTask> = tasks.into_iter().map(LiveTask::new).collect();
        Self {
            board: Arc::new(Mutex::new(live_tasks)),
            storage_path,
            lease_ttl: Duration::from_secs(ttl),
        }
    }

    /// Derive the `.taskboard` file path from a `.chat` file path.
    pub fn taskboard_path_from_chat(chat_path: &std::path::Path) -> PathBuf {
        chat_path.with_extension("taskboard")
    }

    /// Returns the command info for the TUI command palette without needing
    /// an instantiated plugin. Used by `all_known_commands()`.
    pub fn default_commands() -> Vec<CommandInfo> {
        vec![CommandInfo {
            name: "taskboard".to_owned(),
            description:
                "Manage task lifecycle — post, list, show, claim, assign, plan, approve, update, release, finish, cancel"
                    .to_owned(),
            usage: "/taskboard <action> [args...]".to_owned(),
            params: vec![
                ParamSchema {
                    name: "action".to_owned(),
                    param_type: ParamType::Choice(vec![
                        "post".to_owned(),
                        "list".to_owned(),
                        "show".to_owned(),
                        "claim".to_owned(),
                        "assign".to_owned(),
                        "plan".to_owned(),
                        "approve".to_owned(),
                        "update".to_owned(),
                        "release".to_owned(),
                        "finish".to_owned(),
                        "cancel".to_owned(),
                    ]),
                    required: true,
                    description: "Subcommand".to_owned(),
                },
                ParamSchema {
                    name: "args".to_owned(),
                    param_type: ParamType::Text,
                    required: false,
                    description: "Task ID or description".to_owned(),
                },
            ],
        }]
    }

    /// Sweep expired leases (lazy — called before reads).
    fn sweep_expired(&self) -> Vec<String> {
        let mut board = self.board.lock().unwrap();
        let ttl = self.lease_ttl.as_secs();
        let mut expired_ids = Vec::new();
        for lt in board.iter_mut() {
            if lt.is_expired(ttl)
                && matches!(
                    lt.task.status,
                    TaskStatus::Claimed | TaskStatus::Planned | TaskStatus::Approved
                )
            {
                let prev_assignee = lt.task.assigned_to.clone().unwrap_or_default();
                expired_ids.push(format!("{} (was {})", lt.task.id, prev_assignee));
                lt.expire();
            }
        }
        if !expired_ids.is_empty() {
            let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
            let _ = task::save_tasks(&self.storage_path, &tasks);
        }
        expired_ids
    }
}

impl Plugin for TaskboardPlugin {
    fn name(&self) -> &str {
        "taskboard"
    }

    fn commands(&self) -> Vec<CommandInfo> {
        Self::default_commands()
    }

    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
        Box::pin(async move {
            let action = ctx.params.first().map(String::as_str).unwrap_or("");
            let (result, broadcast) = match action {
                "post" => self.handle_post(&ctx),
                "list" => (self.handle_list(), false),
                "claim" => self.handle_claim(&ctx),
                "assign" => self.handle_assign(&ctx),
                "plan" => self.handle_plan(&ctx),
                "approve" => self.handle_approve(&ctx),
                "show" => (self.handle_show(&ctx), false),
                "update" => self.handle_update(&ctx),
                "release" => self.handle_release(&ctx),
                "finish" => self.handle_finish(&ctx),
                "cancel" => self.handle_cancel(&ctx),
                "" => ("usage: /taskboard <post|list|show|claim|assign|plan|approve|update|release|finish|cancel> [args...]".to_owned(), false),
                other => (format!("unknown action: {other}. use: post, list, show, claim, assign, plan, approve, update, release, finish, cancel"), false),
            };
            if broadcast {
                // Emit a typed event alongside the system broadcast.
                let event_type = match action {
                    "post" => Some(EventType::TaskPosted),
                    "claim" => Some(EventType::TaskClaimed),
                    "assign" => Some(EventType::TaskAssigned),
                    "plan" => Some(EventType::TaskPlanned),
                    "approve" => Some(EventType::TaskApproved),
                    "update" => Some(EventType::TaskUpdated),
                    "release" => Some(EventType::TaskReleased),
                    "finish" => Some(EventType::TaskFinished),
                    "cancel" => Some(EventType::TaskCancelled),
                    _ => None,
                };
                if let Some(et) = event_type {
                    let _ = ctx.writer.emit_event(et, &result, None).await;
                }
                Ok(PluginResult::Broadcast(result))
            } else {
                Ok(PluginResult::Reply(result))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_plugin() -> (TaskboardPlugin, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let plugin = TaskboardPlugin::new(tmp.path().to_path_buf(), Some(600));
        (plugin, tmp)
    }

    #[test]
    fn plugin_name() {
        let (plugin, _tmp) = make_plugin();
        assert_eq!(plugin.name(), "taskboard");
    }

    #[test]
    fn plugin_commands() {
        let (plugin, _tmp) = make_plugin();
        let cmds = plugin.commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "taskboard");
        assert_eq!(cmds[0].params.len(), 2);
        if let ParamType::Choice(ref choices) = cmds[0].params[0].param_type {
            assert!(choices.contains(&"post".to_owned()));
            assert!(choices.contains(&"approve".to_owned()));
            assert!(choices.contains(&"assign".to_owned()));
            assert_eq!(choices.len(), 11);
        } else {
            panic!("expected Choice param type");
        }
    }

    #[test]
    fn taskboard_path_from_chat_replaces_extension() {
        let chat = PathBuf::from("/data/room-dev.chat");
        let tb = TaskboardPlugin::taskboard_path_from_chat(&chat);
        assert_eq!(tb, PathBuf::from("/data/room-dev.taskboard"));
    }

    #[test]
    fn default_commands_matches_commands() {
        let (plugin, _tmp) = make_plugin();
        let default = TaskboardPlugin::default_commands();
        let instance = plugin.commands();
        assert_eq!(default.len(), instance.len());
        assert_eq!(default[0].name, instance[0].name);
        assert_eq!(default[0].params.len(), instance[0].params.len());
    }
}
