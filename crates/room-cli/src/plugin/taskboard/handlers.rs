use super::*;

impl TaskboardPlugin {
    pub(super) fn handle_post(&self, ctx: &CommandContext) -> (String, bool) {
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

    pub(super) fn handle_list(&self) -> String {
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
            let desc = if lt.task.description.chars().count() > 40 {
                let truncated: String = lt.task.description.chars().take(37).collect();
                format!("{truncated}...")
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

    pub(super) fn handle_claim(&self, ctx: &CommandContext) -> (String, bool) {
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

    pub(super) fn handle_plan(&self, ctx: &CommandContext) -> (String, bool) {
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
            format!("task {task_id} plan submitted — awaiting approval\nplan: {plan_text}"),
            true,
        )
    }

    pub(super) fn handle_approve(&self, ctx: &CommandContext) -> (String, bool) {
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
        // Poster or host can approve.
        let is_poster = lt.task.posted_by == ctx.sender;
        let is_host = ctx.metadata.host.as_deref() == Some(&ctx.sender);
        if !is_poster && !is_host {
            return ("only the task poster or host can approve".to_owned(), false);
        }
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

    pub(super) fn handle_update(&self, ctx: &CommandContext) -> (String, bool) {
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

    pub(super) fn handle_release(&self, ctx: &CommandContext) -> (String, bool) {
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

    pub(super) fn handle_assign(&self, ctx: &CommandContext) -> (String, bool) {
        let task_id = match ctx.params.get(1) {
            Some(id) => id,
            None => {
                return (
                    "usage: /taskboard assign <task-id> <username>".to_owned(),
                    false,
                )
            }
        };
        let target_user = match ctx.params.get(2) {
            Some(u) => u,
            None => {
                return (
                    "usage: /taskboard assign <task-id> <username>".to_owned(),
                    false,
                )
            }
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
                    "task {task_id} is {} (must be open to assign)",
                    lt.task.status
                ),
                false,
            );
        }
        // Only poster or host can assign.
        let is_poster = lt.task.posted_by == ctx.sender;
        let is_host = ctx.metadata.host.as_deref() == Some(&ctx.sender);
        if !is_poster && !is_host {
            return ("only the task poster or host can assign".to_owned(), false);
        }
        lt.task.status = TaskStatus::Claimed;
        lt.task.assigned_to = Some(target_user.clone());
        lt.task.claimed_at = Some(chrono::Utc::now());
        lt.renew_lease();
        let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let _ = task::save_tasks(&self.storage_path, &tasks);
        (
            format!("task {task_id} assigned to {target_user} by {}", ctx.sender),
            true,
        )
    }

    pub(super) fn handle_show(&self, ctx: &CommandContext) -> String {
        let task_id = match ctx.params.get(1) {
            Some(id) => id,
            None => return "usage: /taskboard show <task-id>".to_owned(),
        };
        self.sweep_expired();
        let board = self.board.lock().unwrap();
        let lt = match board.iter().find(|lt| lt.task.id == *task_id) {
            Some(lt) => lt,
            None => return format!("task {task_id} not found"),
        };
        let t = &lt.task;
        let assignee = t.assigned_to.as_deref().unwrap_or("-");
        let plan = t.plan.as_deref().unwrap_or("-");
        let approved_by = t.approved_by.as_deref().unwrap_or("-");
        let notes = t.notes.as_deref().unwrap_or("-");
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
        format!(
            "task {}\n  status:      {}\n  description: {}\n  posted by:   {}\n  assigned to: {}\n  plan:        {}\n  approved by: {}\n  notes:       {}\n  lease:       {}",
            t.id, t.status, t.description, t.posted_by, assignee, plan, approved_by, notes, elapsed
        )
    }

    pub(super) fn handle_finish(&self, ctx: &CommandContext) -> (String, bool) {
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

    pub(super) fn handle_cancel(&self, ctx: &CommandContext) -> (String, bool) {
        let task_id = match ctx.params.get(1) {
            Some(id) => id,
            None => {
                return (
                    "usage: /taskboard cancel <task-id> [reason]".to_owned(),
                    false,
                )
            }
        };
        self.sweep_expired();
        let mut board = self.board.lock().unwrap();
        let lt = match board.iter_mut().find(|lt| lt.task.id == *task_id) {
            Some(lt) => lt,
            None => return (format!("task {task_id} not found"), false),
        };
        if matches!(lt.task.status, TaskStatus::Finished | TaskStatus::Cancelled) {
            return (
                format!("task {task_id} is {} (cannot cancel)", lt.task.status),
                false,
            );
        }
        // Permission: poster, assignee, or host can cancel.
        let is_poster = lt.task.posted_by == ctx.sender;
        let is_assignee = lt.task.assigned_to.as_deref() == Some(&ctx.sender);
        let is_host = ctx.metadata.host.as_deref() == Some(&ctx.sender);
        if !is_poster && !is_assignee && !is_host {
            return (
                format!("task {task_id} can only be cancelled by the poster, assignee, or host"),
                false,
            );
        }
        lt.task.status = TaskStatus::Cancelled;
        lt.lease_start = None;
        let reason: String = ctx
            .params
            .iter()
            .skip(2)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        lt.task.notes = Some(if reason.is_empty() {
            format!("cancelled by {}", ctx.sender)
        } else {
            format!("cancelled by {}: {reason}", ctx.sender)
        });
        lt.task.updated_at = Some(chrono::Utc::now());
        let tasks: Vec<Task> = board.iter().map(|lt| lt.task.clone()).collect();
        let _ = task::save_tasks(&self.storage_path, &tasks);
        let msg = if reason.is_empty() {
            format!("task {task_id} cancelled by {}", ctx.sender)
        } else {
            format!("task {task_id} cancelled by {} — {reason}", ctx.sender)
        };
        (msg, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_plugin() -> (TaskboardPlugin, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let plugin = TaskboardPlugin::new(tmp.path().to_path_buf(), Some(600));
        (plugin, tmp)
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
        assert!(result.contains("plan: add struct, write tests"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Planned);
        assert_eq!(
            board[0].task.plan.as_deref(),
            Some("add struct, write tests")
        );
    }

    #[test]
    fn handle_approve_by_poster() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        plugin.handle_plan(&test_ctx("agent", &["plan", "tb-001", "my plan"]));
        // Poster (ba) can approve without being host.
        let (result, broadcast) =
            plugin.handle_approve(&test_ctx_with_host("ba", &["approve", "tb-001"], None));
        assert!(result.contains("approved"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Approved);
    }

    #[test]
    fn handle_approve_by_host() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        plugin.handle_plan(&test_ctx("agent", &["plan", "tb-001", "my plan"]));
        // Host can approve even if not the poster.
        let (result, broadcast) = plugin.handle_approve(&test_ctx_with_host(
            "joao",
            &["approve", "tb-001"],
            Some("joao"),
        ));
        assert!(result.contains("approved"));
        assert!(broadcast);
    }

    #[test]
    fn handle_approve_rejected_for_non_poster_non_host() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        plugin.handle_plan(&test_ctx("agent", &["plan", "tb-001", "my plan"]));
        // Random user (not poster, not host) cannot approve.
        let (result, broadcast) = plugin.handle_approve(&test_ctx_with_host(
            "random",
            &["approve", "tb-001"],
            Some("joao"),
        ));
        assert!(result.contains("only the task poster or host"));
        assert!(!broadcast);
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
    fn handle_show_displays_full_detail() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "build the feature"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        plugin.handle_plan(&test_ctx("agent", &["plan", "tb-001", "add struct, tests"]));
        let result = plugin.handle_show(&test_ctx("anyone", &["show", "tb-001"]));
        assert!(result.contains("tb-001"));
        assert!(result.contains("planned"));
        assert!(result.contains("build the feature"));
        assert!(result.contains("agent"));
        assert!(result.contains("add struct, tests"));
        assert!(result.contains("ba")); // posted by
    }

    #[test]
    fn handle_show_not_found() {
        let (plugin, _tmp) = make_plugin();
        let result = plugin.handle_show(&test_ctx("a", &["show", "tb-999"]));
        assert!(result.contains("not found"));
    }

    #[test]
    fn handle_show_no_args() {
        let (plugin, _tmp) = make_plugin();
        let result = plugin.handle_show(&test_ctx("a", &["show"]));
        assert!(result.contains("usage"));
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
        // post -> claim -> plan -> approve -> update -> finish
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
    fn handle_assign_happy_path() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "implement feature"]));
        let (result, broadcast) = plugin.handle_assign(&test_ctx_with_host(
            "ba",
            &["assign", "tb-001", "agent"],
            None,
        ));
        assert!(result.contains("assigned to agent"));
        assert!(result.contains("by ba"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Claimed);
        assert_eq!(board[0].task.assigned_to.as_deref(), Some("agent"));
    }

    #[test]
    fn handle_assign_by_host() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        // Host (joao) can assign even though ba posted it.
        let (result, broadcast) = plugin.handle_assign(&test_ctx_with_host(
            "joao",
            &["assign", "tb-001", "saphire"],
            Some("joao"),
        ));
        assert!(result.contains("assigned to saphire"));
        assert!(result.contains("by joao"));
        assert!(broadcast);
    }

    #[test]
    fn handle_assign_rejected_non_poster_non_host() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        let (result, broadcast) = plugin.handle_assign(&test_ctx_with_host(
            "random",
            &["assign", "tb-001", "agent"],
            Some("joao"),
        ));
        assert!(result.contains("only the task poster or host"));
        assert!(!broadcast);
    }

    #[test]
    fn handle_assign_wrong_status() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        // Task is already claimed — assign should fail.
        let (result, broadcast) =
            plugin.handle_assign(&test_ctx("ba", &["assign", "tb-001", "other"]));
        assert!(result.contains("must be open to assign"));
        assert!(!broadcast);
    }

    #[test]
    fn handle_assign_not_found() {
        let (plugin, _tmp) = make_plugin();
        let (result, broadcast) =
            plugin.handle_assign(&test_ctx("ba", &["assign", "tb-999", "agent"]));
        assert!(result.contains("not found"));
        assert!(!broadcast);
    }

    #[test]
    fn handle_assign_missing_args() {
        let (plugin, _tmp) = make_plugin();
        // No task ID.
        let (result, broadcast) = plugin.handle_assign(&test_ctx("ba", &["assign"]));
        assert!(result.contains("usage"));
        assert!(!broadcast);
        // No username.
        let (result, broadcast) = plugin.handle_assign(&test_ctx("ba", &["assign", "tb-001"]));
        assert!(result.contains("usage"));
        assert!(!broadcast);
    }

    #[test]
    fn handle_assign_then_plan_and_finish() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "implement #502"]));
        // Assign to agent.
        plugin.handle_assign(&test_ctx("ba", &["assign", "tb-001", "agent"]));
        // Agent can submit plan on assigned task.
        let (result, broadcast) = plugin.handle_plan(&test_ctx(
            "agent",
            &["plan", "tb-001", "add handler and tests"],
        ));
        assert!(result.contains("plan submitted"));
        assert!(broadcast);
        // Approve and finish.
        plugin.handle_approve(&test_ctx_with_host(
            "ba",
            &["approve", "tb-001"],
            Some("ba"),
        ));
        let (result, broadcast) = plugin.handle_finish(&test_ctx("agent", &["finish", "tb-001"]));
        assert!(result.contains("finished"));
        assert!(broadcast);
    }

    #[test]
    fn handle_cancel_by_poster() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "obsolete task"]));
        let (result, broadcast) =
            plugin.handle_cancel(&test_ctx("ba", &["cancel", "tb-001", "no longer needed"]));
        assert!(result.contains("cancelled by ba"));
        assert!(result.contains("no longer needed"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Cancelled);
        assert!(board[0]
            .task
            .notes
            .as_deref()
            .unwrap()
            .contains("no longer needed"));
        assert!(board[0].lease_start.is_none());
    }

    #[test]
    fn handle_cancel_by_assignee() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        let (result, broadcast) = plugin.handle_cancel(&test_ctx("agent", &["cancel", "tb-001"]));
        assert!(result.contains("cancelled by agent"));
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.status, TaskStatus::Cancelled);
        assert!(board[0]
            .task
            .notes
            .as_deref()
            .unwrap()
            .contains("cancelled by agent"));
    }

    #[test]
    fn handle_cancel_by_host() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        // Host (joao) can cancel even if not poster or assignee.
        let (result, broadcast) = plugin.handle_cancel(&test_ctx_with_host(
            "joao",
            &["cancel", "tb-001", "scope changed"],
            Some("joao"),
        ));
        assert!(result.contains("cancelled by joao"));
        assert!(result.contains("scope changed"));
        assert!(broadcast);
    }

    #[test]
    fn handle_cancel_finished_rejected() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        plugin.handle_finish(&test_ctx("agent", &["finish", "tb-001"]));
        let (result, broadcast) = plugin.handle_cancel(&test_ctx("ba", &["cancel", "tb-001"]));
        assert!(result.contains("cannot cancel"));
        assert!(result.contains("finished"));
        assert!(!broadcast);
    }

    #[test]
    fn handle_cancel_unauthorized_rejected() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        plugin.handle_claim(&test_ctx("agent", &["claim", "tb-001"]));
        // Random user who is neither poster, assignee, nor host.
        let (result, broadcast) = plugin.handle_cancel(&test_ctx_with_host(
            "random",
            &["cancel", "tb-001"],
            Some("joao"),
        ));
        assert!(result.contains("poster, assignee, or host"));
        assert!(!broadcast);
    }

    #[test]
    fn handle_cancel_no_reason() {
        let (plugin, _tmp) = make_plugin();
        plugin.handle_post(&test_ctx("ba", &["post", "task"]));
        let (result, broadcast) = plugin.handle_cancel(&test_ctx("ba", &["cancel", "tb-001"]));
        assert!(result.contains("cancelled by ba"));
        assert!(!result.contains("\u{2014}")); // no reason separator
        assert!(broadcast);
        let board = plugin.board.lock().unwrap();
        assert_eq!(board[0].task.notes.as_deref(), Some("cancelled by ba"));
    }

    /// Regression test: descriptions with multibyte UTF-8 characters (emoji, CJK)
    /// longer than 40 chars must not panic on truncation. Before the fix,
    /// `&description[..37]` would panic with "byte index is not a char boundary".
    #[test]
    fn handle_list_multibyte_description_does_not_panic() {
        let (plugin, _tmp) = make_plugin();
        // 41 emoji characters — each is 4 bytes, so byte index 37 falls mid-char.
        let emoji_desc = "\u{1F680}".repeat(41); // x 41
        plugin.handle_post(&test_ctx("ba", &["post", &emoji_desc]));

        // CJK characters (3 bytes each).
        let cjk_desc = "\u{4E16}\u{754C}".repeat(25); // x 25 = 50 chars
        plugin.handle_post(&test_ctx("ba", &["post", &cjk_desc]));

        // Mixed ASCII + emoji that lands the 37th-char boundary mid-codepoint.
        let mixed = format!(
            "{}\u{1F3AF}\u{1F3AF}\u{1F3AF}\u{1F3AF}\u{1F3AF}",
            "a".repeat(35)
        ); // 35 ASCII + 5 emoji = 40 chars
        plugin.handle_post(&test_ctx("ba", &["post", &mixed]));

        // This is the call that panicked before the fix.
        let result = plugin.handle_list();

        assert!(result.contains("tb-001"));
        assert!(result.contains("tb-002"));
        assert!(result.contains("tb-003"));
        assert!(
            result.contains("..."),
            "long descriptions should be truncated with ..."
        );
    }

    // -- Test helpers -----------------------------------------------------------

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
            history: Box::new(crate::plugin::HistoryReader::new(
                std::path::Path::new("/dev/null"),
                sender,
            )),
            writer: Box::new(writer),
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
