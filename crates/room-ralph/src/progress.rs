//! Progress file management — read/write structured progress files
//! that survive context exhaustion.
//!
//! Port of the progress file logic from ralph-room.sh to Rust.
//! Owner: bumblebee (bb) — tests and implementation refinement.

use std::path::{Path, PathBuf};

/// Returns the path to the progress file for an issue or username.
pub fn progress_file_path(issue: Option<&str>, username: &str) -> PathBuf {
    match issue {
        Some(i) if !i.is_empty() => PathBuf::from(format!("/tmp/room-progress-{i}.md")),
        _ => PathBuf::from(format!("/tmp/room-progress-{username}.md")),
    }
}

/// Read an existing progress file, returning its contents or None.
pub fn read_progress(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Write a structured progress file on context exhaustion.
pub fn write_progress(
    path: &Path,
    iteration: u32,
    issue: Option<&str>,
    response: &str,
) -> std::io::Result<()> {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let issue_str = issue.unwrap_or("unassigned");

    // Truncate response to last 50 lines
    let truncated: Vec<&str> = response.lines().rev().take(50).collect();
    let truncated: Vec<&str> = truncated.into_iter().rev().collect();
    let truncated_text = truncated.join("\n");

    let content = format!(
        "# Progress — {ts}\n\
         \n\
         ## Metadata\n\
         - Iteration: {iteration}\n\
         - Issue: {issue_str}\n\
         - Reason: context exhaustion\n\
         \n\
         ## Last output (truncated)\n\
         ```\n\
         {truncated_text}\n\
         ```\n\
         \n\
         ## Status\n\
         Context exhausted. Restarting with fresh context.\n"
    );

    std::fs::write(path, content)
}

/// Delete a progress file (cleanup after PR merge).
pub fn delete_progress(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
    } else {
        Ok(())
    }
}

/// Append a usage log entry to the progress file.
/// Delegates formatting to monitor module constants but handles I/O here.
pub fn log_usage_to_file(
    path: &Path,
    input_tokens: u64,
    output_tokens: u64,
    iteration: u32,
) -> std::io::Result<()> {
    crate::monitor::log_usage(path, input_tokens, output_tokens, iteration)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── progress_file_path ──────────────────────────────────────────

    #[test]
    fn progress_file_path_with_issue() {
        let path = progress_file_path(Some("42"), "saphire");
        assert_eq!(path, PathBuf::from("/tmp/room-progress-42.md"));
    }

    #[test]
    fn progress_file_path_without_issue_uses_username() {
        let path = progress_file_path(None, "saphire");
        assert_eq!(path, PathBuf::from("/tmp/room-progress-saphire.md"));
    }

    #[test]
    fn progress_file_path_empty_issue_uses_username() {
        let path = progress_file_path(Some(""), "bb");
        assert_eq!(path, PathBuf::from("/tmp/room-progress-bb.md"));
    }

    // ── read_progress ───────────────────────────────────────────────

    #[test]
    fn read_progress_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");
        std::fs::write(&path, "# Progress\nSome content").unwrap();

        let content = read_progress(&path);
        assert!(content.is_some());
        assert!(content.unwrap().contains("Some content"));
    }

    #[test]
    fn read_progress_missing_file_returns_none() {
        let path = Path::new("/tmp/nonexistent-bb-test-progress-8f3a.md");
        assert!(read_progress(path).is_none());
    }

    #[test]
    fn read_progress_empty_file_returns_some_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");
        std::fs::write(&path, "").unwrap();

        let content = read_progress(&path);
        assert!(content.is_some());
        assert!(content.unwrap().is_empty());
    }

    // ── write_progress ──────────────────────────────────────────────

    #[test]
    fn write_progress_creates_file_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        write_progress(&path, 3, Some("42"), "some claude output").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Progress"));
        assert!(content.contains("Iteration: 3"));
        assert!(content.contains("Issue: 42"));
        assert!(content.contains("context exhaustion"));
    }

    #[test]
    fn write_progress_without_issue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        write_progress(&path, 1, None, "output").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Issue: unassigned"));
    }

    #[test]
    fn write_progress_truncates_long_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        let lines: Vec<String> = (1..=100).map(|i| format!("line {}", i)).collect();
        let long_response = lines.join("\n");

        write_progress(&path, 1, Some("99"), &long_response).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        // Last 50 lines (51-100) should be present
        assert!(content.contains("line 100"));
        assert!(content.contains("line 51"));
        // Line 50 is the boundary — should NOT be present (only last 50 kept)
        assert!(!content.contains("\nline 50\n"));
    }

    #[test]
    fn write_progress_short_response_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        write_progress(&path, 1, Some("1"), "short\nresponse").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("short"));
        assert!(content.contains("response"));
    }

    #[test]
    fn write_progress_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        write_progress(&path, 1, Some("1"), "first").unwrap();
        write_progress(&path, 2, Some("1"), "second").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Iteration: 2"));
        assert!(!content.contains("Iteration: 1"));
    }

    // ── delete_progress ─────────────────────────────────────────────

    #[test]
    fn delete_progress_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");
        std::fs::write(&path, "content").unwrap();

        delete_progress(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn delete_progress_nonexistent_is_ok() {
        let path = Path::new("/tmp/nonexistent-bb-test-delete-9c7b.md");
        assert!(delete_progress(path).is_ok());
    }

    // ── log_usage_to_file ───────────────────────────────────────────

    #[test]
    fn log_usage_to_file_delegates_to_monitor() {
        std::env::remove_var("CONTEXT_LIMIT");
        std::env::remove_var("CONTEXT_THRESHOLD");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        log_usage_to_file(&path, 150_000, 2_000, 3).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("## Context Usage"));
        assert!(content.contains("input=150000"));
        assert!(content.contains("iter=3"));
    }

    // ── write then read round-trip ──────────────────────────────────

    #[test]
    fn write_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.md");

        write_progress(&path, 5, Some("199"), "implementing monitor.rs").unwrap();

        let content = read_progress(&path).unwrap();
        assert!(content.contains("Iteration: 5"));
        assert!(content.contains("Issue: 199"));
        assert!(content.contains("implementing monitor.rs"));
    }
}
