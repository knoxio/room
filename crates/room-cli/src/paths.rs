//! Room filesystem path resolution.
//!
//! All persistent state lives under `~/.room/`:
//! - `~/.room/state/` — tokens, cursors, subscriptions (0700)
//! - `~/.room/data/`  — chat files (default, overridable via `--data-dir`)
//!
//! Ephemeral runtime files (sockets, PID, meta) use the platform-native
//! temporary directory:
//! - macOS: `$TMPDIR` (per-user, e.g. `/var/folders/...`)
//! - Linux: `$XDG_RUNTIME_DIR/room/` or `/tmp/` fallback

use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;

// ── Public path accessors ─────────────────────────────────────────────────────

/// Root of all persistent room state: `~/.room/`.
pub fn room_home() -> PathBuf {
    home_dir().join(".room")
}

/// Directory for persistent state files (tokens, cursors, subscriptions).
///
/// Returns `~/.room/state/`.
pub fn room_state_dir() -> PathBuf {
    room_home().join("state")
}

/// Default directory for chat files: `~/.room/data/`.
///
/// Overridable at daemon startup with `--data-dir`.
pub fn room_data_dir() -> PathBuf {
    room_home().join("data")
}

/// Platform-native socket path for the multi-room daemon.
///
/// - macOS: `$TMPDIR/roomd.sock`
/// - Linux: `$XDG_RUNTIME_DIR/room/roomd.sock` (falls back to `/tmp/roomd.sock`)
pub fn room_socket_path() -> PathBuf {
    runtime_dir().join("roomd.sock")
}

/// Resolve the effective daemon socket path.
///
/// Resolution order:
/// 1. `explicit` — caller-supplied path (e.g. from `--socket` flag).
/// 2. `ROOM_SOCKET` environment variable.
/// 3. Platform-native default (`room_socket_path()`).
pub fn effective_socket_path(explicit: Option<&std::path::Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_owned();
    }
    if let Ok(p) = std::env::var("ROOM_SOCKET") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    room_socket_path()
}

/// Platform-native socket path for a single-room broker.
pub fn room_single_socket_path(room_id: &str) -> PathBuf {
    runtime_dir().join(format!("room-{room_id}.sock"))
}

/// Platform-native meta file path for a single-room broker.
pub fn room_meta_path(room_id: &str) -> PathBuf {
    runtime_dir().join(format!("room-{room_id}.meta"))
}

/// Token file path for a given room/user pair.
///
/// Returns `~/.room/state/room-<room_id>-<username>.token`.
pub fn token_path(room_id: &str, username: &str) -> PathBuf {
    room_state_dir().join(format!("room-{room_id}-{username}.token"))
}

/// Cursor file path for a given room/user pair.
///
/// Returns `~/.room/state/room-<room_id>-<username>.cursor`.
pub fn cursor_path(room_id: &str, username: &str) -> PathBuf {
    room_state_dir().join(format!("room-{room_id}-{username}.cursor"))
}

/// Broker token-map file path: `<state_dir>/<room_id>.tokens`.
///
/// The broker persists its in-memory `TokenMap` here on every token issuance.
pub fn broker_tokens_path(state_dir: &Path, room_id: &str) -> PathBuf {
    state_dir.join(format!("{room_id}.tokens"))
}

/// PID file for the daemon process: `~/.room/roomd.pid`.
///
/// Written by `ensure_daemon_running` when it auto-starts the daemon.
/// Ephemeral — deleted on clean daemon shutdown, may linger after a crash.
pub fn room_pid_path() -> PathBuf {
    room_home().join("roomd.pid")
}

/// System-level token persistence path: `~/.room/state/tokens.json`.
///
/// Tokens in a daemon are system-level — a single token issued by `room join`
/// in any room is valid in all rooms managed by the same daemon. This file
/// stores the complete token → username mapping across all rooms.
pub fn system_tokens_path() -> PathBuf {
    room_state_dir().join("tokens.json")
}

/// Directory that contained per-room token files in older daemon versions.
///
/// Before `~/.room/state/` was introduced, `room join` wrote token files to
/// the platform-native runtime directory (`$TMPDIR` on macOS,
/// `$XDG_RUNTIME_DIR/room/` or `/tmp/` on Linux). The daemon scans this
/// directory on every startup to import any tokens that pre-date the
/// `~/.room/state/` migration, so existing clients do not need to re-join.
pub fn legacy_token_dir() -> PathBuf {
    runtime_dir()
}

/// Broker subscription-map file path: `<state_dir>/<room_id>.subscriptions`.
///
/// The broker persists per-user subscription tiers here on every mutation
/// (subscribe, unsubscribe, auto-subscribe on @mention).
pub fn broker_subscriptions_path(state_dir: &Path, room_id: &str) -> PathBuf {
    state_dir.join(format!("{room_id}.subscriptions"))
}

// ── Directory initialisation ──────────────────────────────────────────────────

/// Ensure `~/.room/state/` and `~/.room/data/` exist.
///
/// Both directories are created with mode `0700` on Unix to protect token
/// files from other users on the same machine. `recursive(true)` means the
/// call is idempotent — safe to call on every daemon/broker start.
pub fn ensure_room_dirs() -> std::io::Result<()> {
    create_dir_0700(&room_state_dir())?;
    create_dir_0700(&room_data_dir())?;
    Ok(())
}

// ── Internals ────────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn runtime_dir() -> PathBuf {
    // macOS: $TMPDIR is per-user and secure (/var/folders/...)
    // Linux: prefer $XDG_RUNTIME_DIR if set, fall back to /tmp
    #[cfg(target_os = "macos")]
    {
        std::env::var("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var("XDG_RUNTIME_DIR")
            .map(|d| PathBuf::from(d).join("room"))
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
    }
}

fn create_dir_0700(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_home_ends_with_dot_room() {
        let h = room_home();
        assert!(
            h.ends_with(".room"),
            "expected path ending in .room, got: {h:?}"
        );
    }

    #[test]
    fn room_state_dir_under_room_home() {
        assert!(room_state_dir().starts_with(room_home()));
        assert!(room_state_dir().ends_with("state"));
    }

    #[test]
    fn room_data_dir_under_room_home() {
        assert!(room_data_dir().starts_with(room_home()));
        assert!(room_data_dir().ends_with("data"));
    }

    #[test]
    fn token_path_is_per_room_and_user() {
        let alice_r1 = token_path("room1", "alice");
        let bob_r1 = token_path("room1", "bob");
        let alice_r2 = token_path("room2", "alice");
        assert_ne!(alice_r1, bob_r1);
        assert_ne!(alice_r1, alice_r2);
        assert!(alice_r1.to_str().unwrap().contains("alice"));
        assert!(alice_r1.to_str().unwrap().contains("room1"));
    }

    #[test]
    fn cursor_path_is_per_room_and_user() {
        let p = cursor_path("myroom", "bob");
        assert!(p.to_str().unwrap().contains("bob"));
        assert!(p.to_str().unwrap().contains("myroom"));
        assert!(p.to_str().unwrap().ends_with(".cursor"));
    }

    #[test]
    fn broker_tokens_path_contains_room_id() {
        let base = PathBuf::from("/tmp/state");
        let p = broker_tokens_path(&base, "test-room");
        assert_eq!(p, base.join("test-room.tokens"));
    }

    #[test]
    fn broker_subscriptions_path_contains_room_id() {
        let base = PathBuf::from("/tmp/state");
        let p = broker_subscriptions_path(&base, "test-room");
        assert_eq!(p, base.join("test-room.subscriptions"));
    }

    #[test]
    fn create_dir_0700_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("nested").join("deep");
        create_dir_0700(&target).unwrap();
        // Second call must not error (recursive=true).
        create_dir_0700(&target).unwrap();
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn create_dir_0700_sets_correct_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("secret");
        create_dir_0700(&target).unwrap();
        let perms = std::fs::metadata(&target).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o700,
            "expected 0700, got {:o}",
            perms.mode() & 0o777
        );
    }

    // ── effective_socket_path ─────────────────────────────────────────────

    #[test]
    fn effective_socket_path_uses_env_var() {
        // This test must not conflict with other env-dependent tests running in
        // parallel.  We snapshot and restore ROOM_SOCKET around the assertion.
        let key = "ROOM_SOCKET";
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "/tmp/test-roomd.sock");
        let result = effective_socket_path(None);
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        assert_eq!(result, PathBuf::from("/tmp/test-roomd.sock"));
    }

    #[test]
    fn effective_socket_path_explicit_overrides_env() {
        let key = "ROOM_SOCKET";
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "/tmp/env-roomd.sock");
        let explicit = PathBuf::from("/tmp/explicit.sock");
        let result = effective_socket_path(Some(&explicit));
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        assert_eq!(result, explicit);
    }

    #[test]
    fn effective_socket_path_default_without_env() {
        // Only test when ROOM_SOCKET is not set in the current environment.
        if std::env::var("ROOM_SOCKET").is_err() {
            let result = effective_socket_path(None);
            assert_eq!(result, room_socket_path());
        }
    }

    #[test]
    fn legacy_token_dir_returns_valid_path() {
        let p = legacy_token_dir();
        // Must be absolute and non-empty.
        assert!(p.is_absolute(), "expected absolute path, got: {p:?}");
    }

    #[test]
    fn ensure_room_dirs_creates_state_and_data() {
        // We cannot call ensure_room_dirs() directly without writing to ~/.room,
        // so test the underlying create_dir_0700 with a temp directory.
        let dir = tempfile::TempDir::new().unwrap();
        let state = dir.path().join("state");
        let data = dir.path().join("data");
        create_dir_0700(&state).unwrap();
        create_dir_0700(&data).unwrap();
        assert!(state.exists());
        assert!(data.exists());
    }
}
