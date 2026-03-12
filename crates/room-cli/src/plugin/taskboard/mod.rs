pub mod task;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use task::{next_id, LiveTask, Task, TaskStatus};

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
                "Manage task lifecycle — post, claim, plan, approve, update, release, finish"
                    .to_owned(),
            usage: "/taskboard <action> [args...]".to_owned(),
            params: vec![
                ParamSchema {
                    name: "action".to_owned(),
                    param_type: ParamType::Choice(vec![
                        "post".to_owned(),
                        "list".to_owned(),
                        "claim".to_owned(),
                        "plan".to_owned(),
                        "approve".to_owned(),
                        "update".to_owned(),
                        "release".to_owned(),
                        "finish".to_owned(),
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

    fn handle_post(&self, ctx: &CommandContext) -> (String, bool) {
        let description = ctx.params[1..].join(" ");
        if description.is_empty() {
            return ("usage: /taskboard post <description>".to_owned(), false);
        }
        let mut board = self.board.lock().unwrap();
        let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let id = next_id(&tasks);
        let task = Task {
            id: id.clone(),
            description: description.clone(),
            status: TaskStatus::Open,
            posted_by: ctx.sender.clone(),
            assigned_to: None,
            posted_at: chrono::Utc::now(),
            claimed_at: None,
            plan: None,
            approved_by: None,
            approved_at: None,
            updated_at: None,
            notes: None,
        };
        board.push(LiveTask::new(task.clone()));
        let all_tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let _ = task::save_tasks(&self.storage_path, &all_tasks);
        (format!("task {id} posted: {description}"), true)
    }

    fn handle_list(&self) -> String {
        let expired = self.sweep_expired();
        let board = self.board.lock().unwrap();
        if board.is_empty() {
            return "taskboard is empty".to_owned();
        }
        let mut lines = Vec::new();
        if !expired.is_empty() {
            lines.push(format!("expired: {}", expired.join(", ")));
        }
        lines.push(format!(
            "{:<8} {:<10} {:<12} {:<12} {}",
            "ID", "STATUS", "ASSIGNEE", "ELAPSED", "DESCRIPTION"
        ));
        for lt in board.iter() {
            let elapsed = match lt.lease_start {
                Some(start) => {
                    let secs = start.elapsed().as_secs();
                    if secs < 60 {
                        format!("{secs}s")
                    } else {
                        format!("{}m", secs / 60)
                    }
                }
                None => "-".to_owned(),
            };
            let assignee = lt.task.assigned_to.as_deref().unwrap_or("-").to_owned();
            let desc = if lt.task.description.len() > 40 {
                format!("{}...", &lt.task.description[..37])
            } else {
                lt.task.description.clone()
            };
            lines.push(format!(
                "{:<8} {:<10} {:<12} {:<12} {}",
                lt.task.id, lt.task.status, assignee, elapsed, desc
            ));
        }
        lines.join("\n")
    }

    fn handle_claim(&self, ctx: &CommandContext) -> (String, bool) {
        let task_id = match ctx.params.get(1) {
            Some(id) => id,
            None => return ("usage: /taskboard claim <task-id>".to_owned(), false),
        };
        self.sweep_expired();
        let mut board = self.board.lock().unwrap();
        let lt = match board.iter_mut().find(|lt| lt.task.id == *task_id) {
            Some(lt) => lt,
            None => return (format!("task {task_id} not found"), false),
        };
        if lt.task.status != TaskStatus::Open {
            return (
                format!(
                    "task {task_id} is {} (must be open to claim)",
                    lt.task.status
                ),
                false,
            );
        }
        lt.task.status = TaskStatus::Claimed;
        lt.task.assigned_to = Some(ctx.sender.clone());
        lt.task.claimed_at = Some(chrono::Utc::now());
        lt.renew_lease();
        let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let _ = task::save_tasks(&self.storage_path, &tasks);
        (
            format!(
                "task {task_id} claimed by {} — submit plan with /taskboard plan {task_id} <plan>",
                ctx.sender
            ),
            true,
        )
    }

    fn handle_plan(&self, ctx: &CommandContext) -> (String, bool) {
        let task_id = match ctx.params.get(1) {
            Some(id) => id,
            None => {
                return (
                    "usage: /taskboard plan <task-id> <plan text>".to_owned(),
                    false,
                )
            }
        };
        let plan_text = ctx.params[2..].join(" ");
        if plan_text.is_empty() {
            return (
                "usage: /taskboard plan <task-id> <plan text>".to_owned(),
                false,
            );
        }
        self.sweep_expired();
        let mut board = self.board.lock().unwrap();
        let lt = match board.iter_mut().find(|lt| lt.task.id == *task_id) {
            Some(lt) => lt,
            None => return (format!("task {task_id} not found"), false),
        };
        if !matches!(lt.task.status, TaskStatus::Claimed | TaskStatus::Planned) {
            return (
                format!(
                    "task {task_id} is {} (must be claimed to submit plan)",
                    lt.task.status
                ),
                false,
            );
        }
        if lt.task.assigned_to.as_deref() != Some(&ctx.sender) {
            return (format!("task {task_id} is assigned to someone else"), false);
        }
        lt.task.status = TaskStatus::Planned;
        lt.task.plan = Some(plan_text.clone());
        lt.renew_lease();
        let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let _ = task::save_tasks(&self.storage_path, &tasks);
        (
            format!("task {task_id} plan submitted — awaiting approval"),
            true,
        )
    }

    fn handle_approve(&self, ctx: &CommandContext) -> (String, bool) {
        let task_id = match ctx.params.get(1) {
            Some(id) => id,
            None => return ("usage: /taskboard approve <task-id>".to_owned(), false),
        };
        self.sweep_expired();
        let mut board = self.board.lock().unwrap();
        let lt = match board.iter_mut().find(|lt| lt.task.id == *task_id) {
            Some(lt) => lt,
            None => return (format!("task {task_id} not found"), false),
        };
        if lt.task.status != TaskStatus::Planned {
            return (
                format!(
                    "task {task_id} is {} (must be planned to approve)",
                    lt.task.status
                ),
                false,
            );
        }
        // Only host can approve — checked via metadata in handle().
        lt.task.status = TaskStatus::Approved;
        lt.task.approved_by = Some(ctx.sender.clone());
        lt.task.approved_at = Some(chrono::Utc::now());
        lt.renew_lease();
        let assignee = lt
            .task
            .assigned_to
            .as_deref()
            .unwrap_or("unknown")
            .to_owned();
        let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let _ = task::save_tasks(&self.storage_path, &tasks);
        (
            format!(
                "task {task_id} approved by {} — @{assignee} proceed with implementation",
                ctx.sender
            ),
            true,
        )
    }

    fn handle_update(&self, ctx: &CommandContext) -> (String, bool) {
        let task_id = match ctx.params.get(1) {
            Some(id) => id,
            None => {
                return (
                    "usage: /taskboard update <task-id> [notes]".to_owned(),
                    false,
                )
            }
        };
        let notes = if ctx.params.len() > 2 {
            Some(ctx.params[2..].join(" "))
        } else {
            None
        };
        self.sweep_expired();
        let mut board = self.board.lock().unwrap();
        let lt = match board.iter_mut().find(|lt| lt.task.id == *task_id) {
            Some(lt) => lt,
            None => return (format!("task {task_id} not found"), false),
        };
        if !matches!(
            lt.task.status,
            TaskStatus::Claimed | TaskStatus::Planned | TaskStatus::Approved
        ) {
            return (
                format!(
                    "task {task_id} is {} (must be claimed/planned/approved to update)",
                    lt.task.status
                ),
                false,
            );
        }
        if lt.task.assigned_to.as_deref() != Some(&ctx.sender) {
            return (format!("task {task_id} is assigned to someone else"), false);
        }
        let mut warning = String::new();
        if lt.task.status != TaskStatus::Approved {
            warning = format!(" [warning: task is {} — not yet approved]", lt.task.status);
        }
        if let Some(n) = notes {
            lt.task.notes = Some(n);
        }
        lt.renew_lease();
        let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let _ = task::save_tasks(&self.storage_path, &tasks);
        (
            format!("task {task_id} updated, lease renewed{warning}"),
            true,
        )
    }

    fn handle_release(&self, ctx: &CommandContext) -> (String, bool) {
        let task_id = match ctx.params.get(1) {
            Some(id) => id,
            None => return ("usage: /taskboard release <task-id>".to_owned(), false),
        };
        self.sweep_expired();
        let mut board = self.board.lock().unwrap();
        let lt = match board.iter_mut().find(|lt| lt.task.id == *task_id) {
            Some(lt) => lt,
            None => return (format!("task {task_id} not found"), false),
        };
        if !matches!(
            lt.task.status,
            TaskStatus::Claimed | TaskStatus::Planned | TaskStatus::Approved
        ) {
            return (
                format!(
                    "task {task_id} is {} (must be claimed/planned/approved to release)",
                    lt.task.status
                ),
                false,
            );
        }
        // Allow owner or host to release.
        if lt.task.assigned_to.as_deref() != Some(&ctx.sender)
            && ctx.metadata.host.as_deref() != Some(&ctx.sender)
        {
            return (
                format!("task {task_id} can only be released by the assignee or host"),
                false,
            );
        }
        let prev = lt.task.assigned_to.clone().unwrap_or_default();
        lt.task.status = TaskStatus::Open;
        lt.task.assigned_to = None;
        lt.task.claimed_at = None;
        lt.task.plan = None;
        lt.task.approved_by = None;
        lt.task.approved_at = None;
        lt.lease_start = None;
        let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let _ = task::save_tasks(&self.storage_path, &tasks);
        (
            format!("task {task_id} released by {prev} — back to open"),
            true,
        )
    }

    fn handle_finish(&self, ctx: &CommandContext) -> (String, bool) {
        let task_id = match ctx.params.get(1) {
            Some(id) => id,
            None => return ("usage: /taskboard finish <task-id>".to_owned(), false),
        };
        self.sweep_expired();
        let mut board = self.board.lock().unwrap();
        let lt = match board.iter_mut().find(|lt| lt.task.id == *task_id) {
            Some(lt) => lt,
            None => return (format!("task {task_id} not found"), false),
        };
        if !matches!(
            lt.task.status,
            TaskStatus::Claimed | TaskStatus::Planned | TaskStatus::Approved
        ) {
            return (
                format!(
                    "task {task_id} is {} (must be claimed/planned/approved to finish)",
                    lt.task.status
                ),
                false,
            );
        }
        if lt.task.assigned_to.as_deref() != Some(&ctx.sender) {
            return (
                format!("task {task_id} can only be finished by the assignee"),
                false,
            );
        }
        lt.task.status = TaskStatus::Finished;
        lt.lease_start = None;
        lt.task.updated_at = Some(chrono::Utc::now());
        let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let _ = task::save_tasks(&self.storage_path, &tasks);
        (format!("task {task_id} finished by {}", ctx.sender), true)
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
                "plan" => self.handle_plan(&ctx),
                "approve" => {
                    // Only host can approve.
                    if ctx.metadata.host.as_deref() != Some(&ctx.sender) {
                        ("only the host can approve tasks".to_owned(), false)
                    } else {
                        self.handle_approve(&ctx)
                    }
                }
                "update" => self.handle_update(&ctx),
                "release" => self.handle_release(&ctx),
                "finish" => self.handle_finish(&ctx),
                "" => ("usage: /taskboard <post|list|claim|plan|approve|update|release|finish> [args...]".to_owned(), false),
                other => (format!("unknown action: {other}. use: post, list, claim, plan, approve, update, release, finish"), false),
            };
            if broadcast {
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
            assert_eq!(choices.len(), 8);
        } else {
            panic!("expected Choice param type");
        }
    }

    #[test]
    fn handle_post_creates_task() {
        let (plugin, _tmp) = make_plugin();
        let ctx = test_ctx("alice", &["post", "fix the bug"]);
        let (result, broadcast) = plugin.handle_post(&ctx);
        assert!(result.contains("tb-001"));
        assert!(result.contains("fix the bug"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].task.status, TaskStatus::Open);
    }

    #[test]
    fn handle_post_empty_description() {
        let (plugin, _tmp) = make_plugin();
        let ctx = test_ctx("alice", &["post"]);
        let (result, broadcast) = plugin.handle_post(&ctx);
        assert!(result.contains("usage"));
        assert!(!broadcast);
    }

    #[test]
    fn handle_claim_and_plan_flow() {
        let (plugin, _tmp) = make_plugin();
        // Post a task.
        plugin.handle_post(&test_ctx("ba", &["post", "implement feature"]));
        // Claim it.
        let (result, broadcast) = plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        assert!(result.contains("claimed by agent"));
        assert!(broadcast);
        // Submit plan.
        let (result, broadcast) = plugin.handle_plan(&test_ctx(
            "agent",
            &["plan", "tb-001", "add struct, write tests"],
        ));
        assert!(result.contains("plan submitted"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Planned);
        assert_eq!(
            board[0].task.plan.as_deref(),
            Some("add struct, write tests")
        );
    }

    #[test]
    fn handle_approve_requires_host() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        plugin.handle_plan(&test_ctx("agent", &["plan", "tb-001", "my plan"]));
        // Non-host approve should be caught in Plugin::handle, but test the method directly.
        let (result, broadcast) = plugin.handle_approve(&test_ctx_with_host(
            "ba",
            &["approve", "tb-001"],
            Some("ba"),
        ));
        assert!(result.contains("approved"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Approved);
    }

    #[test]
    fn handle_update_renews_lease() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        let (result, broadcast) =
            plugin.handle_update(&test_ctx("agent", &["update", "tb-001", "progress note"]));
        assert!(result.contains("lease renewed"));
        assert!(result.contains("warning")); // not approved yet
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.notes.as_deref(), Some("progress note"));
    }

    #[test]
    fn handle_update_no_warning_when_approved() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        plugin.handle_plan(&test_ctx("agent", &["plan", "tb-001", "plan"]));
        plugin.handle_approve(&test_ctx_with_host(
            "ba",
            &["approve", "tb-001"],
            Some("ba"),
        ));
        let (result, broadcast) = plugin.handle_update(&test_ctx("agent", &["update", "tb-001"]));
        assert!(result.contains("lease renewed"));
        assert!(!result.contains("warning"));
        assert!(broadcast);
    }

    #[test]
    fn handle_release_back_to_open() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        let (result, broadcast) = plugin.handle_release(&test_ctx("agent", &["release", "tb-001"]));
        assert!(result.contains("released"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Open);
        assert!(board[0].task.assigned_to.is_none());
    }

    #[test]
    fn handle_finish() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        let (result, broadcast) = plugin.handle_finish(&test_ctx("agent", &["finish", "tb-001"]));
        assert!(result.contains("finished"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Finished);
    }

    #[test]
    fn handle_claim_wrong_status() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("a", &["claim", "tb-001"]));
        let (result, broadcast) = plugin.handle_claim(&test_ctx("b", &["claim", "tb-001"]));
        assert!(result.contains("must be open"));
        assert!(!broadcast);
    }

    #[test]
    fn handle_plan_wrong_user() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent-a", &["claim", "tb-001"]));
        let (result, broadcast) =
            plugin.handle_plan(&test_ctx("agent-b", &["plan", "tb-001", "my plan"]));
        assert!(result.contains("assigned to someone else"));
        assert!(!broadcast);
    }

    #[test]
    fn handle_list_shows_tasks() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "first task"]));
        plugin.handle_post(&test_ctx("ba", &["post", "second task"]));
        let result = plugin.handle_list();
        assert!(result.contains("tb-001"));
        assert!(result.contains("tb-002"));
        assert!(result.contains("first task"));
    }

    #[test]
    fn handle_list_empty() {
        let (plugin, _tmp) = make_plugin();
        let result = plugin.handle_list();
        assert_eq!(result, "taskboard is empty");
    }

    #[test]
    fn handle_not_found() {
        let (plugin, _tmp) = make_plugin();
        let (result, broadcast) = plugin.handle_claim(&test_ctx("a", &["claim", "tb-999"]));
        assert!(result.contains("not found"));
        assert!(!broadcast);
    }

    #[test]
    fn persistence_survives_reload() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        {
            let plugin = TaskboardPlugin::new(path.clone(), Some(600));
            plugin.handle_post(&test_ctx("ba", &["post", "persistent task"]));
            plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        }
        // Reload from disk.
        let plugin2 = TaskboardPlugin::new(path, Some(600));
        let board = plugin2.board.lock().unwrap();
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].task.id, "tb-001");
        assert_eq!(board[0].task.status, TaskStatus::Claimed);
    }

    #[test]
    fn lease_expiry_on_list() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        // Force lease to the past.
        {
            let mut board = plugin.board.lock().unwrap();
            board[0].lease_start =
                Some(std::time::Instant::now() - std::time::Duration::from_secs(700));
        }
        let result = plugin.handle_list();
        assert!(result.contains("expired"));
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Open);
    }

    #[test]
    fn full_lifecycle() {
        let (plugin, _tmp) = make_plugin();
        // post → claim → plan → approve → update → finish
        plugin.handle_post(&test_ctx("ba", &["post", "implement #42"]));
        plugin.handle_claim(&test_ctx("saphire", &["claim", "tb-001"]));
        plugin.handle_plan(&test_ctx(
            "saphire",
            &["plan", "tb-001", "add Foo, write tests"],
        ));
        plugin.handle_approve(&test_ctx_with_host(
            "ba",
            &["approve", "tb-001"],
            Some("ba"),
        ));
        plugin.handle_update(&test_ctx("saphire", &["update", "tb-001", "tests passing"]));
        plugin.handle_finish(&test_ctx("saphire", &["finish", "tb-001"]));
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Finished);
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

    // ── Test helpers ────────────────────────────────────────────────────────

    fn test_ctx(sender: &str, params: &[&str]) -> CommandContext {
        test_ctx_with_host(sender, params, None)
    }

    fn test_ctx_with_host(sender: &str, params: &[&str], host: Option<&str>) -> CommandContext {
        use std::collections::HashMap;
        use std::sync::atomic::AtomicU64;

        use crate::plugin::{ChatWriter, RoomMetadata, UserInfo};

        let clients = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let chat_path = Arc::new(PathBuf::from("/dev/null"));
        let room_id = Arc::new("test-room".to_owned());
        let seq_counter = Arc::new(AtomicU64::new(0));
        let writer = ChatWriter::new(&clients, &chat_path, &room_id, &seq_counter, "taskboard");

        CommandContext {
            command: "taskboard".to_owned(),
            params: params.iter().map(|s| s.to_string()).collect(),
            sender: sender.to_owned(),
            room_id: "test-room".to_owned(),
            message_id: "msg-001".to_owned(),
            timestamp: chrono::Utc::now(),
            history: crate::plugin::HistoryReader::new(std::path::Path::new("/dev/null"), sender),
            writer,
            metadata: RoomMetadata {
                online_users: vec![UserInfo {
                    username: sender.to_owned(),
                    status: String::new(),
                }],
                host: host.map(|h| h.to_owned()),
                message_count: 0,
            },
            available_commands: vec![],
        }
    }
}
