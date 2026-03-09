use std::path::{Path, PathBuf};

use super::transport::global_join_session;
use crate::paths;

/// One-shot join subcommand: register username globally, receive token, write token file.
///
/// Writes to `~/.room/state/room-<username>.token`. The token is global —
/// not tied to any specific room. Use `room subscribe <room>` to join rooms.
///
/// If the username is already registered, returns the existing token.
///
/// `socket` overrides the default socket path (auto-discovered if `None`).
pub async fn cmd_join(username: &str, socket: Option<&std::path::Path>) -> anyhow::Result<()> {
    paths::ensure_room_dirs().map_err(|e| anyhow::anyhow!("cannot create ~/.room: {e}"))?;
    let socket_path = paths::effective_socket_path(socket);
    let (returned_user, token) = global_join_session(&socket_path, username).await?;
    let token_data = serde_json::json!({"username": returned_user, "token": token});
    let token_path = paths::global_token_path(&returned_user);
    std::fs::write(&token_path, format!("{token_data}\n"))?;
    println!("{token_data}");
    Ok(())
}

/// Look up the username associated with `token` by scanning global token files.
///
/// Scans `~/.room/state/room-<username>.token` files and returns the username
/// whose token matches. Used by `poll`, `watch`, and `dm` to resolve the caller's
/// identity without requiring a username argument.
pub fn username_from_token(token: &str) -> anyhow::Result<String> {
    let state_dir = paths::room_state_dir();
    let prefix = "room-";
    let suffix = ".token";
    let files: Vec<PathBuf> = std::fs::read_dir(&state_dir)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", state_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix) && n.ends_with(suffix))
                .unwrap_or(false)
        })
        .collect();

    for path in files {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                if v["token"].as_str() == Some(token) {
                    if let Some(u) = v["username"].as_str() {
                        return Ok(u.to_owned());
                    }
                }
            }
        }
    }

    anyhow::bail!("token not recognised — run: room join <username> to get a fresh token")
}

/// Read the cursor position from disk, returning `None` if the file is absent or empty.
pub fn read_cursor(cursor_path: &Path) -> Option<String> {
    std::fs::read_to_string(cursor_path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Persist the cursor position to disk.
///
/// Creates parent directories if they do not exist (e.g. `~/.room/state/` on first run).
pub fn write_cursor(cursor_path: &Path, id: &str) -> anyhow::Result<()> {
    if let Some(parent) = cursor_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(cursor_path, id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write a global token file into a temp dir.
    fn write_token_file(dir: &std::path::Path, username: &str, token: &str) {
        let name = format!("room-{username}.token");
        let data = serde_json::json!({"username": username, "token": token});
        fs::write(dir.join(name), format!("{data}\n")).unwrap();
    }

    /// A version of username_from_token that scans a custom directory (for hermetic tests).
    fn username_from_token_in(dir: &std::path::Path, token: &str) -> anyhow::Result<String> {
        let prefix = "room-";
        let suffix = ".token";
        let files: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(prefix) && n.ends_with(suffix))
                    .unwrap_or(false)
            })
            .collect();

        for path in files {
            if let Ok(data) = fs::read_to_string(&path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                    if v["token"].as_str() == Some(token) {
                        if let Some(u) = v["username"].as_str() {
                            return Ok(u.to_owned());
                        }
                    }
                }
            }
        }
        anyhow::bail!("token not recognised — run: room join <username>")
    }

    #[test]
    fn finds_correct_user() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "alice", "tok-alice");
        let user = username_from_token_in(dir.path(), "tok-alice").unwrap();
        assert_eq!(user, "alice");
    }

    #[test]
    fn disambiguates_multiple_users() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "alice", "tok-alice");
        write_token_file(dir.path(), "bob", "tok-bob");

        assert_eq!(
            username_from_token_in(dir.path(), "tok-alice").unwrap(),
            "alice"
        );
        assert_eq!(
            username_from_token_in(dir.path(), "tok-bob").unwrap(),
            "bob"
        );
    }

    #[test]
    fn unknown_token_errors_with_join_hint() {
        let dir = TempDir::new().unwrap();
        let err = username_from_token_in(dir.path(), "not-a-real-token").unwrap_err();
        assert!(
            err.to_string().contains("room join"),
            "expected 'room join' hint in: {err}"
        );
    }

    #[test]
    fn two_agents_tokens_do_not_collide() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "alice", "tok-alice");
        write_token_file(dir.path(), "bob", "tok-bob");

        assert_eq!(
            username_from_token_in(dir.path(), "tok-alice").unwrap(),
            "alice"
        );
        assert_eq!(
            username_from_token_in(dir.path(), "tok-bob").unwrap(),
            "bob"
        );
    }

    #[test]
    fn ignores_non_token_files() {
        let dir = TempDir::new().unwrap();
        // Write a non-token file that matches the prefix
        fs::write(dir.path().join("room-lobby.sock"), "not a token").unwrap();
        write_token_file(dir.path(), "bob", "tok-bob");

        assert_eq!(
            username_from_token_in(dir.path(), "tok-bob").unwrap(),
            "bob"
        );
    }

    #[test]
    fn read_cursor_returns_none_when_file_absent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cursor");
        assert!(read_cursor(&path).is_none());
    }

    #[test]
    fn write_then_read_cursor_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cursor");
        write_cursor(&path, "abc-123").unwrap();
        assert_eq!(read_cursor(&path).unwrap(), "abc-123");
    }
}
