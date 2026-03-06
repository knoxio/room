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

    #[test]
    fn progress_file_path_with_issue() {
        assert_eq!(
            progress_file_path(Some("42"), "agent"),
            PathBuf::from("/tmp/room-progress-42.md")
        );
    }

    #[test]
    fn progress_file_path_without_issue() {
        assert_eq!(
            progress_file_path(None, "saphire"),
            PathBuf::from("/tmp/room-progress-saphire.md")
        );
    }

    #[test]
    fn progress_file_path_empty_issue() {
        assert_eq!(
            progress_file_path(Some(""), "agent"),
            PathBuf::from("/tmp/room-progress-agent.md")
        );
    }

    #[test]
    fn write_and_read_progress() {
        let path = PathBuf::from("/tmp/test-ralph-progress-wr.md");
        write_progress(&path, 3, Some("99"), "line1\nline2\nline3").unwrap();
        let content = read_progress(&path).unwrap();
        assert!(content.contains("Iteration: 3"));
        assert!(content.contains("Issue: 99"));
        assert!(content.contains("line1"));
        assert!(content.contains("context exhaustion"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_progress_nonexistent() {
        assert!(read_progress(Path::new("/tmp/nonexistent-ralph-test.md")).is_none());
    }

    #[test]
    fn delete_progress_file() {
        let path = PathBuf::from("/tmp/test-ralph-progress-del.md");
        std::fs::write(&path, "test").unwrap();
        assert!(path.exists());
        delete_progress(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn delete_progress_nonexistent_is_ok() {
        assert!(delete_progress(Path::new("/tmp/nonexistent-ralph-del.md")).is_ok());
    }

    #[test]
    fn write_progress_truncates_long_response() {
        let path = PathBuf::from("/tmp/test-ralph-progress-trunc.md");
        let long_response: String = (0..100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        write_progress(&path, 1, None, &long_response).unwrap();
        let content = read_progress(&path).unwrap();
        // Should contain the last 50 lines, not all 100
        assert!(content.contains("line 99"));
        assert!(content.contains("line 50"));
        assert!(!content.contains("line 0\n"));
        std::fs::remove_file(&path).ok();
    }
}
