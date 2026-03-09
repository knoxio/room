use std::path::{Path, PathBuf};

use super::transport::{join_session_target, resolve_socket_target};
use crate::paths;

/// Returns the canonical token file path for a given room/user pair.
///
/// Resolves to `~/.room/state/room-<room_id>-<username>.token`.
/// One file per (room, user) pair — multiple agents on the same machine never
/// overwrite each other's tokens.
pub fn token_file_path(room_id: &str, username: &str) -> PathBuf {
    paths::token_path(room_id, username)
}

/// One-shot join subcommand: register username, receive token, write token file.
///
/// Writes to `~/.room/state/room-<room_id>-<username>.token` so agents sharing
/// a machine do not clobber each other. Subsequent `send`, `poll`, and `watch`
/// calls find the file automatically (single-agent) or via `--user <username>`
/// (multi-agent).
///
/// `socket` overrides the default socket path (auto-discovered if `None`).
pub async fn cmd_join(
    room_id: &str,
    username: &str,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    paths::ensure_room_dirs().map_err(|e| anyhow::anyhow!("cannot create ~/.room: {e}"))?;
    let target = resolve_socket_target(room_id, socket);
    let (returned_user, token) = join_session_target(&target, username).await?;
    let token_data = serde_json::json!({"username": returned_user, "token": token});
    let token_path = token_file_path(room_id, &returned_user);
    std::fs::write(&token_path, format!("{token_data}\n"))?;
    println!("{token_data}");
    Ok(())
}

/// Look up the username associated with `token` by scanning stored token files for `room_id`.
///
/// `room join` writes `~/.room/state/room-<room_id>-<username>.token` for each session.
/// This function finds the file whose `token` field matches the given value and
/// returns the corresponding username. Used by `poll` and `watch` to resolve the
/// cursor file path without requiring the caller to pass a username explicitly.
pub fn username_from_token(room_id: &str, token: &str) -> anyhow::Result<String> {
    let state_dir = paths::room_state_dir();
    let prefix = format!("room-{room_id}-");
    let suffix = ".token";
    let files: Vec<PathBuf> = std::fs::read_dir(&state_dir)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", state_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&prefix) && n.ends_with(suffix))
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

    anyhow::bail!("token not recognised — run: room join {room_id} <username> to get a fresh token")
}

/// Look up the username associated with `token` by scanning ALL token files in the state dir.
///
/// Unlike [`username_from_token`], this does not require a room_id. It scans every
/// `room-*-*.token` file in `~/.room/state/` and returns the username from the first match.
/// Used by `room dm` where the caller's room_id is unknown (the DM room_id depends on the
/// caller's username, creating a chicken-and-egg problem).
pub fn username_from_token_any_room(token: &str) -> anyhow::Result<String> {
    let state_dir = crate::paths::room_state_dir();
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

    anyhow::bail!("token not recognised — run: room join <room-id> <username> to get a fresh token")
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

    /// Write a token file into a temp dir.
    fn write_token_file(dir: &std::path::Path, room_id: &str, username: &str, token: &str) {
        let name = format!("room-{room_id}-{username}.token");
        let data = serde_json::json!({"username": username, "token": token});
        fs::write(dir.join(name), format!("{data}\n")).unwrap();
    }

    /// A version of username_from_token that scans a custom directory (for hermetic tests).
    fn username_from_token_in(
        dir: &std::path::Path,
        room_id: &str,
        token: &str,
    ) -> anyhow::Result<String> {
        let prefix = format!("room-{room_id}-");
        let suffix = ".token";
        let files: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(&prefix) && n.ends_with(suffix))
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
        anyhow::bail!("token not recognised — run: room join {room_id} <username>")
    }

    #[test]
    fn token_file_path_is_per_user() {
        let alice = token_file_path("myroom", "alice");
        let bob = token_file_path("myroom", "bob");
        assert_ne!(alice, bob);
        assert!(alice.to_str().unwrap().contains("alice"));
        assert!(bob.to_str().unwrap().contains("bob"));
    }

    #[test]
    fn username_from_token_finds_correct_user() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "r1", "alice", "tok-alice");
        let user = username_from_token_in(dir.path(), "r1", "tok-alice").unwrap();
        assert_eq!(user, "alice");
    }

    #[test]
    fn username_from_token_disambiguates_multiple_users() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "r2", "alice", "tok-alice");
        write_token_file(dir.path(), "r2", "bob", "tok-bob");

        assert_eq!(
            username_from_token_in(dir.path(), "r2", "tok-alice").unwrap(),
            "alice"
        );
        assert_eq!(
            username_from_token_in(dir.path(), "r2", "tok-bob").unwrap(),
            "bob"
        );
    }

    #[test]
    fn username_from_token_unknown_errors_with_join_hint() {
        let dir = TempDir::new().unwrap();
        let err = username_from_token_in(dir.path(), "r3", "not-a-real-token").unwrap_err();
        assert!(
            err.to_string().contains("room join"),
            "expected 'room join' hint in: {err}"
        );
    }

    #[test]
    fn two_agents_tokens_do_not_collide() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "r4", "alice", "tok-alice");
        write_token_file(dir.path(), "r4", "bob", "tok-bob");

        assert_eq!(
            username_from_token_in(dir.path(), "r4", "tok-alice").unwrap(),
            "alice"
        );
        assert_eq!(
            username_from_token_in(dir.path(), "r4", "tok-bob").unwrap(),
            "bob"
        );
    }

    /// A version of username_from_token_any_room that scans a custom directory.
    fn username_from_token_any_room_in(
        dir: &std::path::Path,
        token: &str,
    ) -> anyhow::Result<String> {
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
        anyhow::bail!("token not recognised")
    }

    #[test]
    fn any_room_finds_token_across_rooms() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "lobby", "alice", "tok-alice");
        write_token_file(dir.path(), "dev", "alice", "tok-alice-dev");

        assert_eq!(
            username_from_token_any_room_in(dir.path(), "tok-alice").unwrap(),
            "alice"
        );
        assert_eq!(
            username_from_token_any_room_in(dir.path(), "tok-alice-dev").unwrap(),
            "alice"
        );
    }

    #[test]
    fn any_room_unknown_token_errors() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "lobby", "alice", "tok-alice");
        let err = username_from_token_any_room_in(dir.path(), "bogus").unwrap_err();
        assert!(
            err.to_string().contains("token not recognised"),
            "expected error hint in: {err}"
        );
    }

    #[test]
    fn any_room_ignores_non_token_files() {
        let dir = TempDir::new().unwrap();
        // Write a non-token file that matches partially
        fs::write(dir.path().join("room-lobby.sock"), "not a token").unwrap();
        write_token_file(dir.path(), "lobby", "bob", "tok-bob");

        assert_eq!(
            username_from_token_any_room_in(dir.path(), "tok-bob").unwrap(),
            "bob"
        );
    }

    #[test]
    fn any_room_same_user_different_tokens_per_room() {
        let dir = TempDir::new().unwrap();
        write_token_file(dir.path(), "r1", "carol", "tok-r1");
        write_token_file(dir.path(), "r2", "carol", "tok-r2");

        // Both tokens resolve to carol
        assert_eq!(
            username_from_token_any_room_in(dir.path(), "tok-r1").unwrap(),
            "carol"
        );
        assert_eq!(
            username_from_token_any_room_in(dir.path(), "tok-r2").unwrap(),
            "carol"
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
