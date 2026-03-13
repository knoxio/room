//! Agent process cleanup utilities.
//!
//! Provides PID-file–based tracking, process termination with grace periods,
//! and orphan reaping for spawned agent processes. This module is a standalone
//! utility — the AgentPlugin wires it into broker lifecycle hooks.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Record of a spawned agent process, persisted to JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRecord {
    pub pid: u32,
    pub username: String,
    pub room_id: String,
    pub spawned_at: DateTime<Utc>,
}

/// Result of a single process termination attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminateResult {
    /// Process was terminated with SIGTERM.
    Terminated,
    /// Process required SIGKILL after grace period.
    Killed,
    /// Process was already dead.
    AlreadyDead,
}

/// Summary of a cleanup operation.
#[derive(Debug, Clone, Default)]
pub struct CleanupSummary {
    pub terminated: Vec<String>,
    pub killed: Vec<String>,
    pub already_dead: Vec<String>,
}

impl CleanupSummary {
    /// Total number of agents processed.
    pub fn total(&self) -> usize {
        self.terminated.len() + self.killed.len() + self.already_dead.len()
    }
}

/// Load agent records from a JSON file.
///
/// Returns an empty vec if the file is missing, unreadable, or contains
/// invalid JSON. Uses synchronous I/O per codebase convention.
pub fn load_agent_records(path: &Path) -> Vec<AgentRecord> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_str::<Vec<AgentRecord>>(&contents) {
        Ok(records) => records,
        Err(e) => {
            eprintln!(
                "[agent_cleanup] corrupt agent state at {}: {e}",
                path.display()
            );
            Vec::new()
        }
    }
}

/// Save agent records to a JSON file (full rewrite).
///
/// Uses synchronous I/O per codebase convention (avoids tokio::fs
/// cancellation on shutdown).
pub fn save_agent_records(path: &Path, records: &[AgentRecord]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(records)
        .map_err(|e| format!("serialize agent records: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Remove the agent state file (best-effort cleanup).
pub fn remove_agent_state(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Check whether a process with the given PID is currently running.
///
/// Uses POSIX `kill(pid, 0)` — signal 0 checks liveness without
/// delivering a signal.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) never delivers a signal; it only checks liveness.
    let ret = unsafe { libc_kill(pid as i32, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM (errno 1): process exists but we lack permission.
    std::io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    unsafe { kill(pid, sig) }
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    true
}

/// Send SIGTERM to a process.
///
/// Returns `true` if the signal was delivered (or EPERM), `false` if the
/// process does not exist.
#[cfg(unix)]
pub fn send_sigterm(pid: u32) -> bool {
    // SAFETY: SIGTERM (15) is a standard termination signal.
    let ret = unsafe { libc_kill(pid as i32, 15) };
    if ret == 0 {
        return true;
    }
    // EPERM: process exists but we can't signal it.
    std::io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(not(unix))]
pub fn send_sigterm(_pid: u32) -> bool {
    false
}

/// Send SIGKILL to a process.
///
/// Returns `true` if the signal was delivered (or EPERM), `false` if the
/// process does not exist.
#[cfg(unix)]
pub fn send_sigkill(pid: u32) -> bool {
    // SAFETY: SIGKILL (9) is a standard forced-termination signal.
    let ret = unsafe { libc_kill(pid as i32, 9) };
    if ret == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(not(unix))]
pub fn send_sigkill(_pid: u32) -> bool {
    false
}

/// Terminate a single process: SIGTERM, wait up to `grace_secs`, then SIGKILL.
///
/// This is a blocking call (uses `std::thread::sleep` for the grace period).
/// Suitable for shutdown paths where async runtime may be shutting down.
pub fn terminate_process(pid: u32, grace_secs: u64) -> TerminateResult {
    if !pid_alive(pid) {
        return TerminateResult::AlreadyDead;
    }

    // Send SIGTERM.
    send_sigterm(pid);

    // Wait for process to exit, checking every 100ms.
    let checks = (grace_secs * 10).max(1);
    for _ in 0..checks {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !pid_alive(pid) {
            return TerminateResult::Terminated;
        }
    }

    // Still alive — escalate to SIGKILL.
    send_sigkill(pid);

    // Brief wait to confirm.
    std::thread::sleep(std::time::Duration::from_millis(200));
    if pid_alive(pid) {
        // SIGKILL was sent but process is still alive (shouldn't happen).
        TerminateResult::Killed
    } else {
        TerminateResult::Killed
    }
}

/// Clean up a list of agent processes. Returns a summary of actions taken.
pub fn cleanup_agents(records: &[AgentRecord], grace_secs: u64) -> CleanupSummary {
    let mut summary = CleanupSummary::default();
    for record in records {
        let label = format!("{} (pid {})", record.username, record.pid);
        match terminate_process(record.pid, grace_secs) {
            TerminateResult::Terminated => {
                eprintln!("[agent_cleanup] terminated {label}");
                summary.terminated.push(label);
            }
            TerminateResult::Killed => {
                eprintln!("[agent_cleanup] killed {label} (required SIGKILL)");
                summary.killed.push(label);
            }
            TerminateResult::AlreadyDead => {
                eprintln!("[agent_cleanup] {label} already dead");
                summary.already_dead.push(label);
            }
        }
    }
    summary
}

/// Reap orphaned agent processes from a previous broker session.
///
/// Loads the PID state file, checks each process, terminates any that are
/// still alive, and removes the state file when done.
pub fn reap_orphans(state_path: &Path, grace_secs: u64) -> CleanupSummary {
    let records = load_agent_records(state_path);
    if records.is_empty() {
        // No state file or empty — nothing to reap.
        remove_agent_state(state_path);
        return CleanupSummary::default();
    }

    eprintln!(
        "[agent_cleanup] found {} orphaned agent record(s) at {}",
        records.len(),
        state_path.display()
    );

    let summary = cleanup_agents(&records, grace_secs);

    // Remove the stale state file.
    remove_agent_state(state_path);

    eprintln!(
        "[agent_cleanup] orphan reap complete: {} terminated, {} killed, {} already dead",
        summary.terminated.len(),
        summary.killed.len(),
        summary.already_dead.len()
    );

    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_record_serde_round_trip() {
        let record = AgentRecord {
            pid: 12345,
            username: "dev-anna".to_owned(),
            room_id: "room-dev".to_owned(),
            spawned_at: Utc::now(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: AgentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pid, 12345);
        assert_eq!(parsed.username, "dev-anna");
        assert_eq!(parsed.room_id, "room-dev");
    }

    #[test]
    fn agent_records_json_array_round_trip() {
        let records = vec![
            AgentRecord {
                pid: 100,
                username: "agent-a".to_owned(),
                room_id: "room-dev".to_owned(),
                spawned_at: Utc::now(),
            },
            AgentRecord {
                pid: 200,
                username: "agent-b".to_owned(),
                room_id: "room-dev".to_owned(),
                spawned_at: Utc::now(),
            },
        ];
        let tmp = tempfile::NamedTempFile::new().unwrap();
        save_agent_records(tmp.path(), &records).unwrap();
        let loaded = load_agent_records(tmp.path());
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].pid, 100);
        assert_eq!(loaded[1].username, "agent-b");
    }

    #[test]
    fn load_agent_records_missing_file() {
        let records = load_agent_records(Path::new("/nonexistent/agents.json"));
        assert!(records.is_empty());
    }

    #[test]
    fn load_agent_records_corrupt_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "this is not json").unwrap();
        let records = load_agent_records(tmp.path());
        assert!(records.is_empty());
    }

    #[test]
    fn save_and_load_empty_records() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        save_agent_records(tmp.path(), &[]).unwrap();
        let loaded = load_agent_records(tmp.path());
        assert!(loaded.is_empty());
    }

    #[test]
    fn remove_agent_state_missing_file_is_noop() {
        // Should not panic on missing file.
        remove_agent_state(Path::new("/nonexistent/agents.json"));
    }

    #[test]
    fn remove_agent_state_removes_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();
        std::fs::write(&path, "[]").unwrap();
        assert!(path.exists());
        remove_agent_state(&path);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_current_process() {
        // Our own PID should be alive.
        assert!(pid_alive(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_nonexistent_pid() {
        // PID 0 is special (kernel); use a very high PID that almost certainly
        // doesn't exist. PID max on Linux is typically 4194304 or 32768.
        assert!(!pid_alive(4_000_000));
    }

    #[cfg(unix)]
    #[test]
    fn terminate_already_dead_process() {
        let result = terminate_process(4_000_000, 1);
        assert_eq!(result, TerminateResult::AlreadyDead);
    }

    #[test]
    fn cleanup_summary_total() {
        let mut summary = CleanupSummary::default();
        assert_eq!(summary.total(), 0);
        summary.terminated.push("a".to_owned());
        summary.killed.push("b".to_owned());
        summary.already_dead.push("c".to_owned());
        assert_eq!(summary.total(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_agents_with_dead_pids() {
        let records = vec![
            AgentRecord {
                pid: 4_000_000,
                username: "dead-1".to_owned(),
                room_id: "room-dev".to_owned(),
                spawned_at: Utc::now(),
            },
            AgentRecord {
                pid: 4_000_001,
                username: "dead-2".to_owned(),
                room_id: "room-dev".to_owned(),
                spawned_at: Utc::now(),
            },
        ];
        let summary = cleanup_agents(&records, 1);
        assert_eq!(summary.already_dead.len(), 2);
        assert_eq!(summary.terminated.len(), 0);
        assert_eq!(summary.killed.len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn reap_orphans_empty_state() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "[]").unwrap();
        let summary = reap_orphans(tmp.path(), 1);
        assert_eq!(summary.total(), 0);
        // State file should be removed.
        assert!(!tmp.path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn reap_orphans_with_dead_pids() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let records = vec![AgentRecord {
            pid: 4_000_000,
            username: "orphan-1".to_owned(),
            room_id: "room-dev".to_owned(),
            spawned_at: Utc::now(),
        }];
        save_agent_records(tmp.path(), &records).unwrap();
        let summary = reap_orphans(tmp.path(), 1);
        assert_eq!(summary.already_dead.len(), 1);
        assert!(!tmp.path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn reap_orphans_missing_file() {
        let summary = reap_orphans(Path::new("/nonexistent/agents.json"), 1);
        assert_eq!(summary.total(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn send_sigterm_to_nonexistent_pid() {
        assert!(!send_sigterm(4_000_000));
    }

    #[cfg(unix)]
    #[test]
    fn send_sigkill_to_nonexistent_pid() {
        assert!(!send_sigkill(4_000_000));
    }
}
