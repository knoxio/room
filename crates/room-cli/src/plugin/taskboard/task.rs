use std::path::Path;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Status of a task on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    Claimed,
    Planned,
    Approved,
    Finished,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Open => write!(f, "open"),
            TaskStatus::Claimed => write!(f, "claimed"),
            TaskStatus::Planned => write!(f, "planned"),
            TaskStatus::Approved => write!(f, "approved"),
            TaskStatus::Finished => write!(f, "finished"),
        }
    }
}

/// A task on the board, persisted as NDJSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
    pub posted_by: String,
    pub assigned_to: Option<String>,
    pub posted_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub plan: Option<String>,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

/// In-memory task with a lease timestamp for TTL tracking.
///
/// The `lease_start` field is `Instant`-based (monotonic) and is NOT
/// serialized — on load from disk, it is set to `Instant::now()` for
/// claimed/planned/approved tasks.
pub struct LiveTask {
    pub task: Task,
    pub lease_start: Option<Instant>,
}

impl LiveTask {
    pub fn new(task: Task) -> Self {
        let lease_start = match task.status {
            TaskStatus::Claimed | TaskStatus::Planned | TaskStatus::Approved => {
                Some(Instant::now())
            }
            _ => None,
        };
        Self { task, lease_start }
    }

    /// Renew the lease timer (called on claim/plan/update).
    pub fn renew_lease(&mut self) {
        self.lease_start = Some(Instant::now());
        self.task.updated_at = Some(Utc::now());
    }

    /// Check if the lease has expired.
    pub fn is_expired(&self, ttl_secs: u64) -> bool {
        match self.lease_start {
            Some(start) => start.elapsed().as_secs() >= ttl_secs,
            None => false,
        }
    }

    /// Auto-release an expired task back to open status.
    pub fn expire(&mut self) {
        self.task.status = TaskStatus::Open;
        self.task.assigned_to = None;
        self.task.claimed_at = None;
        self.task.plan = None;
        self.task.approved_by = None;
        self.task.approved_at = None;
        self.task.notes = Some("lease expired — auto-released".to_owned());
        self.lease_start = None;
    }
}

/// Load tasks from an NDJSON file. Returns empty vec if the file does not exist.
pub fn load_tasks(path: &Path) -> Vec<Task> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str::<Task>(l) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("[taskboard] corrupt line in {}: {e}", path.display());
                None
            }
        })
        .collect()
}

/// Write all tasks to an NDJSON file (full rewrite).
pub fn save_tasks(path: &Path, tasks: &[Task]) -> Result<(), String> {
    let mut buf = String::new();
    for task in tasks {
        let line =
            serde_json::to_string(task).map_err(|e| format!("serialize task {}: {e}", task.id))?;
        buf.push_str(&line);
        buf.push('\n');
    }
    std::fs::write(path, buf).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Generate the next task ID from the current list.
pub fn next_id(tasks: &[Task]) -> String {
    let max_num = tasks
        .iter()
        .filter_map(|t| t.id.strip_prefix("tb-"))
        .filter_map(|s| s.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("tb-{:03}", max_num + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_task(id: &str, status: TaskStatus) -> Task {
        Task {
            id: id.to_owned(),
            description: "test task".to_owned(),
            status,
            posted_by: "alice".to_owned(),
            assigned_to: None,
            posted_at: Utc::now(),
            claimed_at: None,
            plan: None,
            approved_by: None,
            approved_at: None,
            updated_at: None,
            notes: None,
        }
    }

    #[test]
    fn task_status_display() {
        assert_eq!(TaskStatus::Open.to_string(), "open");
        assert_eq!(TaskStatus::Claimed.to_string(), "claimed");
        assert_eq!(TaskStatus::Planned.to_string(), "planned");
        assert_eq!(TaskStatus::Approved.to_string(), "approved");
        assert_eq!(TaskStatus::Finished.to_string(), "finished");
    }

    #[test]
    fn task_status_serde_round_trip() {
        let task = make_task("tb-001", TaskStatus::Approved);
        let json = serde_json::to_string(&task).unwrap();
        let parsed: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, TaskStatus::Approved);
        assert_eq!(parsed.id, "tb-001");
    }

    #[test]
    fn live_task_lease_starts_for_claimed() {
        let task = make_task("tb-001", TaskStatus::Claimed);
        let live = LiveTask::new(task);
        assert!(live.lease_start.is_some());
    }

    #[test]
    fn live_task_no_lease_for_open() {
        let task = make_task("tb-001", TaskStatus::Open);
        let live = LiveTask::new(task);
        assert!(live.lease_start.is_none());
    }

    #[test]
    fn live_task_no_lease_for_finished() {
        let task = make_task("tb-001", TaskStatus::Finished);
        let live = LiveTask::new(task);
        assert!(live.lease_start.is_none());
    }

    #[test]
    fn live_task_is_expired() {
        let task = make_task("tb-001", TaskStatus::Claimed);
        let mut live = LiveTask::new(task);
        // Force lease to the past.
        live.lease_start = Some(Instant::now() - Duration::from_secs(700));
        assert!(live.is_expired(600));
        assert!(!live.is_expired(900));
    }

    #[test]
    fn live_task_renew_lease() {
        let task = make_task("tb-001", TaskStatus::Claimed);
        let mut live = LiveTask::new(task);
        live.lease_start = Some(Instant::now() - Duration::from_secs(500));
        live.renew_lease();
        assert!(!live.is_expired(600));
        assert!(live.task.updated_at.is_some());
    }

    #[test]
    fn live_task_expire_resets() {
        let mut task = make_task("tb-001", TaskStatus::Approved);
        task.assigned_to = Some("bob".to_owned());
        task.plan = Some("do the thing".to_owned());
        let mut live = LiveTask::new(task);
        live.expire();
        assert_eq!(live.task.status, TaskStatus::Open);
        assert!(live.task.assigned_to.is_none());
        assert!(live.task.plan.is_none());
        assert!(live.lease_start.is_none());
    }

    #[test]
    fn next_id_empty() {
        assert_eq!(next_id(&[]), "tb-001");
    }

    #[test]
    fn next_id_increments() {
        let tasks = vec![
            make_task("tb-001", TaskStatus::Open),
            make_task("tb-005", TaskStatus::Finished),
            make_task("tb-003", TaskStatus::Claimed),
        ];
        assert_eq!(next_id(&tasks), "tb-006");
    }

    #[test]
    fn ndjson_round_trip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let tasks = vec![
            make_task("tb-001", TaskStatus::Open),
            make_task("tb-002", TaskStatus::Claimed),
        ];
        save_tasks(path, &tasks).unwrap();
        let loaded = load_tasks(path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id, "tb-001");
        assert_eq!(loaded[1].id, "tb-002");
        assert_eq!(loaded[1].status, TaskStatus::Claimed);
    }

    #[test]
    fn load_tasks_missing_file() {
        let tasks = load_tasks(Path::new("/nonexistent/path.ndjson"));
        assert!(tasks.is_empty());
    }

    #[test]
    fn load_tasks_skips_corrupt_lines() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let task = make_task("tb-001", TaskStatus::Open);
        let mut content = serde_json::to_string(&task).unwrap();
        content.push('\n');
        content.push_str("this is not json\n");
        let task2 = make_task("tb-002", TaskStatus::Finished);
        content.push_str(&serde_json::to_string(&task2).unwrap());
        content.push('\n');
        std::fs::write(path, content).unwrap();
        let loaded = load_tasks(path);
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn task_status_all_variants_serialize() {
        for status in [
            TaskStatus::Open,
            TaskStatus::Claimed,
            TaskStatus::Planned,
            TaskStatus::Approved,
            TaskStatus::Finished,
        ] {
            let task = make_task("tb-001", status);
            let json = serde_json::to_string(&task).unwrap();
            let parsed: Task = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.status, status);
        }
    }
}
