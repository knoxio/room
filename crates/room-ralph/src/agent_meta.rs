//! Agent metadata file — `.room-agent.json`.
//!
//! Written by room-ralph before spawning claude so the agent can read its
//! identity (username, token, room_id) from a stable file instead of
//! re-joining via `room join`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Metadata written to `.room-agent.json` in the agent's working directory.
///
/// Claude reads this file at the start of each iteration to know its identity
/// without running `room join` (which would create a new user).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMeta {
    /// Assigned username (e.g. "coder-anna").
    pub username: String,
    /// Session token for room CLI commands.
    pub token: String,
    /// Room ID the agent is participating in.
    pub room_id: String,
    /// PID of the room-ralph wrapper process.
    pub ralph_pid: u32,
    /// Path to the daemon socket.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_path: Option<String>,
    /// Personality name (e.g. "coder", "reviewer").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
}

/// Default filename for the metadata file.
pub const META_FILENAME: &str = ".room-agent.json";

/// Write the agent metadata file to the given directory.
pub fn write_meta(dir: &Path, meta: &AgentMeta) -> Result<(), String> {
    let path = dir.join(META_FILENAME);
    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| format!("failed to serialize agent metadata: {e}"))?;
    std::fs::write(&path, format!("{json}\n"))
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(())
}

/// Read agent metadata from the given directory, if the file exists.
pub fn read_meta(dir: &Path) -> Option<AgentMeta> {
    let path = dir.join(META_FILENAME);
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Return the full path to the metadata file in the given directory.
pub fn meta_path(dir: &Path) -> PathBuf {
    dir.join(META_FILENAME)
}

/// Delete the metadata file from the given directory. Ignores missing files.
pub fn cleanup_meta(dir: &Path) {
    let path = dir.join(META_FILENAME);
    std::fs::remove_file(path).ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_meta() -> AgentMeta {
        AgentMeta {
            username: "coder-anna".to_owned(),
            token: "tok-abc123".to_owned(),
            room_id: "dev-room".to_owned(),
            ralph_pid: 12345,
            socket_path: Some("/tmp/roomd.sock".to_owned()),
            personality: Some("coder".to_owned()),
        }
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let meta = sample_meta();
        write_meta(dir.path(), &meta).unwrap();

        let loaded = read_meta(dir.path()).expect("should read back");
        assert_eq!(loaded.username, "coder-anna");
        assert_eq!(loaded.token, "tok-abc123");
        assert_eq!(loaded.room_id, "dev-room");
        assert_eq!(loaded.ralph_pid, 12345);
        assert_eq!(loaded.socket_path.as_deref(), Some("/tmp/roomd.sock"));
        assert_eq!(loaded.personality.as_deref(), Some("coder"));
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        assert!(read_meta(dir.path()).is_none());
    }

    #[test]
    fn cleanup_removes_file() {
        let dir = TempDir::new().unwrap();
        write_meta(dir.path(), &sample_meta()).unwrap();
        assert!(meta_path(dir.path()).exists());

        cleanup_meta(dir.path());
        assert!(!meta_path(dir.path()).exists());
    }

    #[test]
    fn cleanup_missing_file_no_error() {
        let dir = TempDir::new().unwrap();
        cleanup_meta(dir.path()); // should not panic
    }

    #[test]
    fn meta_path_returns_expected() {
        let path = meta_path(Path::new("/home/agent"));
        assert_eq!(path, PathBuf::from("/home/agent/.room-agent.json"));
    }

    #[test]
    fn serialization_skips_none_fields() {
        let meta = AgentMeta {
            username: "test".to_owned(),
            token: "tok".to_owned(),
            room_id: "room".to_owned(),
            ralph_pid: 1,
            socket_path: None,
            personality: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("socket_path"));
        assert!(!json.contains("personality"));
    }

    #[test]
    fn write_to_nonexistent_dir_fails() {
        let result = write_meta(Path::new("/nonexistent/path"), &sample_meta());
        assert!(result.is_err());
    }
}
