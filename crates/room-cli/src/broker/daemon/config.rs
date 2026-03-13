//! Daemon configuration and room ID validation.

use std::path::PathBuf;

/// Characters that are unsafe in filesystem paths or shell contexts.
const UNSAFE_CHARS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'];

/// Maximum allowed length for a room ID.
pub(crate) const MAX_ROOM_ID_LEN: usize = 64;

/// Validate a room ID for filesystem safety.
///
/// Rejects IDs that are empty, too long, contain path traversal sequences
/// (`..`), whitespace, or filesystem-unsafe characters.
pub fn validate_room_id(room_id: &str) -> Result<(), String> {
    if room_id.is_empty() {
        return Err("room ID cannot be empty".into());
    }
    if room_id.len() > MAX_ROOM_ID_LEN {
        return Err(format!(
            "room ID too long ({} chars, max {MAX_ROOM_ID_LEN})",
            room_id.len()
        ));
    }
    if room_id == "." || room_id == ".." || room_id.contains("..") {
        return Err("room ID cannot contain '..'".into());
    }
    if room_id.chars().any(|c| c.is_whitespace()) {
        return Err("room ID cannot contain whitespace".into());
    }
    if let Some(bad) = room_id.chars().find(|c| UNSAFE_CHARS.contains(c)) {
        return Err(format!("room ID contains unsafe character: {bad:?}"));
    }
    Ok(())
}

/// Configuration for the daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Path to the daemon UDS socket (ephemeral, platform-native temp dir).
    pub socket_path: PathBuf,
    /// Directory for chat files. Each room gets `<data_dir>/<room_id>.chat`.
    /// Defaults to `~/.room/data/`; overridable with `--data-dir`.
    pub data_dir: PathBuf,
    /// Directory for state files (token maps, cursors, subscriptions).
    /// Defaults to `~/.room/state/`.
    pub state_dir: PathBuf,
    /// Optional WebSocket/REST port.
    pub ws_port: Option<u16>,
    /// Seconds to wait after the last connection closes before shutting down.
    ///
    /// Default is 30 seconds. Set to 0 for immediate shutdown when the last
    /// client disconnects. Has no effect if there are always active connections.
    pub grace_period_secs: u64,
}

impl DaemonConfig {
    /// Resolve the chat file path for a given room.
    pub fn chat_path(&self, room_id: &str) -> PathBuf {
        self.data_dir.join(format!("{room_id}.chat"))
    }

    /// Resolve the token-map persistence path for a given room.
    pub fn token_map_path(&self, room_id: &str) -> PathBuf {
        crate::paths::broker_tokens_path(&self.state_dir, room_id)
    }

    /// System-level token persistence path: `<state_dir>/tokens.json`.
    ///
    /// Used by the daemon to share a single token store across all rooms.
    /// Production default is `~/.room/state/tokens.json`; tests override
    /// `state_dir` with a temp directory.
    pub fn system_tokens_path(&self) -> PathBuf {
        self.state_dir.join("tokens.json")
    }

    /// Resolve the subscription-map persistence path for a given room.
    pub fn subscription_map_path(&self, room_id: &str) -> PathBuf {
        crate::paths::broker_subscriptions_path(&self.state_dir, room_id)
    }

    /// Resolve the event-filter-map persistence path for a given room.
    pub fn event_filter_map_path(&self, room_id: &str) -> PathBuf {
        crate::paths::broker_event_filters_path(&self.state_dir, room_id)
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: crate::paths::room_socket_path(),
            data_dir: crate::paths::room_data_dir(),
            state_dir: crate::paths::room_state_dir(),
            ws_port: None,
            grace_period_secs: 30,
        }
    }
}
