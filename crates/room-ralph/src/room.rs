use std::path::PathBuf;
use std::process::Command;

use room_protocol::Message;

/// Returns the path to the ralph log file.
pub fn log_file_path(username: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/ralph-room-{username}.log"))
}

/// Returns the path to the global token file (room-independent).
pub fn token_file_path(username: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/room-{username}.token"))
}

/// Returns the path to the legacy per-room token file.
///
/// Used as a fallback when the global token file doesn't exist (e.g. tokens
/// issued before the join/subscribe split).
pub fn legacy_token_file_path(room_id: &str, username: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/room-{room_id}-{username}.token"))
}

/// Result of a successful room join: token UUID and the actual username used.
///
/// The actual username may differ from the requested one if the original was
/// already taken and a numeric suffix was appended (e.g. `c3po` → `c3po-2`).
pub struct JoinResult {
    pub token: String,
    pub username: String,
}

/// Maximum number of suffixed username attempts before giving up.
const MAX_USERNAME_RETRIES: u32 = 5;

/// The environment variable name for pre-provisioned tokens.
///
/// When set, ralph skips `room join` and uses this token directly.
/// Used by the `/agent` plugin spawner to pass a pre-minted token.
pub const ROOM_TOKEN_ENV: &str = "ROOM_TOKEN";

/// Register a user globally and return the token UUID and actual username.
///
/// If the `ROOM_TOKEN` environment variable is set, uses that token directly
/// without running `room join` or `room subscribe` (the spawner is expected to
/// have already provisioned both). Otherwise, runs `room join <username>` and
/// parses the token from JSON output. If the username is already taken, retries
/// with numeric suffixes (e.g. `user-2`, `user-3`, up to 5 attempts). Falls
/// back to cached token file as a last resort — tries the global path first,
/// then the legacy per-room path.
/// If `socket` is provided, passes `--socket <path>` to the `room` command.
pub fn join_room(
    room_id: &str,
    username: &str,
    socket: Option<&str>,
) -> Result<JoinResult, String> {
    // Check for pre-provisioned token from the /agent spawner
    if let Ok(token) = std::env::var(ROOM_TOKEN_ENV) {
        if !token.is_empty() {
            tracing::info!(
                "using pre-provisioned token from {} (skipping room join)",
                ROOM_TOKEN_ENV
            );
            return Ok(JoinResult {
                token,
                username: username.to_owned(),
            });
        }
    }

    // Try the original username first
    match try_join(username, socket) {
        Ok(token) => {
            return Ok(JoinResult {
                token,
                username: username.to_owned(),
            });
        }
        Err(e) if is_username_taken(&e) => {
            tracing::warn!(
                "username '{}' already in use, trying suffixed variants",
                username
            );
        }
        Err(_) => {
            // Non-username error — fall through to cached token
        }
    }

    // Retry with numeric suffixes, then fall back to cached token
    retry_with_suffix(room_id, username, socket)
}

/// Try joining with suffixed usernames (`user-2`, `user-3`, ...). Falls back to
/// the cached token file if all suffixed attempts are exhausted.
fn retry_with_suffix(
    room_id: &str,
    username: &str,
    socket: Option<&str>,
) -> Result<JoinResult, String> {
    for i in 2..=MAX_USERNAME_RETRIES + 1 {
        let suffixed = format!("{username}-{i}");
        match try_join(&suffixed, socket) {
            Ok(token) => {
                tracing::info!(
                    "joined as '{}' (original '{}' was taken)",
                    suffixed,
                    username
                );
                return Ok(JoinResult {
                    token,
                    username: suffixed,
                });
            }
            Err(e) if is_username_taken(&e) => {
                tracing::warn!("username '{}' also taken, trying next suffix", suffixed);
                continue;
            }
            Err(e) => {
                return Err(e);
            }
        }
    }

    // All suffixed attempts exhausted — fall back to cached token (global, then legacy)
    let global_token = token_file_path(username);
    tracing::warn!(
        "all username variants taken, trying cached token at {}",
        global_token.display()
    );
    let token = read_cached_token(&global_token).or_else(|_| {
        let legacy = legacy_token_file_path(room_id, username);
        tracing::warn!(
            "global token not found, trying legacy path at {}",
            legacy.display()
        );
        read_cached_token(&legacy)
    })?;
    Ok(JoinResult {
        token,
        username: username.to_owned(),
    })
}

/// Attempt a single `room join` and return the token or an error string.
fn try_join(username: &str, socket: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("room");
    cmd.args(["join", username]);
    if let Some(s) = socket {
        cmd.args(["--socket", s]);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run `room join`: {e}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        extract_token(&stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(stderr.to_string())
    }
}

/// Check if a join error indicates the username is already taken.
fn is_username_taken(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("username")
        && (lower.contains("already in use") || lower.contains("already taken"))
}

/// Subscribe to a room so that `room poll` and `room watch` deliver messages.
///
/// Runs `room subscribe <room_id> -t <token>`.
/// If `socket` is provided, passes `--socket <path>` to the `room` command.
///
/// This must be called after `join_room()` — joining only registers the user
/// globally and issues a token, but does not subscribe to any specific room.
pub fn subscribe_room(room_id: &str, token: &str, socket: Option<&str>) -> Result<(), String> {
    let mut cmd = Command::new("room");
    cmd.args(["subscribe", room_id, "-t", token]);
    if let Some(s) = socket {
        cmd.args(["--socket", s]);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run `room subscribe`: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("room subscribe failed: {stderr}"))
    }
}

/// Send a message to the room.
///
/// Runs `room send <room_id> -t <token> <message>`.
/// If `socket` is provided, passes `--socket <path>` to the `room` command.
pub fn send_message(
    room_id: &str,
    token: &str,
    message: &str,
    socket: Option<&str>,
) -> Result<(), String> {
    let mut cmd = Command::new("room");
    cmd.args(["send", room_id, "-t", token, message]);
    if let Some(s) = socket {
        cmd.args(["--socket", s]);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run `room send`: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("room send failed: {stderr}"))
    }
}

/// Poll the room for new messages.
///
/// Runs `room poll <room_id> -t <token>` and parses NDJSON output into Messages.
/// Returns `Err` on non-zero exit (e.g. invalid token) so the caller can detect
/// token expiry and re-join.
/// If `socket` is provided, passes `--socket <path>` to the `room` command.
pub fn poll_messages(
    room_id: &str,
    token: &str,
    socket: Option<&str>,
) -> Result<Vec<Message>, String> {
    let mut cmd = Command::new("room");
    cmd.args(["poll", room_id, "-t", token]);
    if let Some(s) = socket {
        cmd.args(["--socket", s]);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run `room poll`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("room poll failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut messages = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Message>(trimmed) {
            Ok(msg) => messages.push(msg),
            Err(e) => tracing::warn!("failed to parse poll message: {e}"),
        }
    }
    Ok(messages)
}

/// Set the agent's status in the room via `/set_status`.
///
/// Delegates to `send_message` with the formatted `/set_status` command.
/// Pass an empty string to clear the status.
/// If `socket` is provided, passes it through to `send_message`.
pub fn set_status(
    room_id: &str,
    token: &str,
    status: &str,
    socket: Option<&str>,
) -> Result<(), String> {
    let message = build_set_status_message(status);
    send_message(room_id, token, &message, socket)
}

fn build_set_status_message(status: &str) -> String {
    if status.is_empty() {
        "/set_status".to_owned()
    } else {
        format!("/set_status {status}")
    }
}

/// Block until a new message arrives in the room, or timeout.
///
/// Runs `room watch <room_id> -t <token> --interval <interval>` which blocks
/// until a foreign message arrives or the process is interrupted. Returns
/// `Ok(true)` if a message arrived, `Ok(false)` on timeout/interrupt, and
/// `Err` on token expiry or other errors.
///
/// If `timeout_secs` is provided, passes `--timeout` to limit the wait.
pub fn watch_room(
    room_id: &str,
    token: &str,
    interval_secs: u64,
    timeout_secs: Option<u64>,
    socket: Option<&str>,
) -> Result<bool, String> {
    let mut cmd = Command::new("room");
    cmd.args([
        "watch",
        room_id,
        "-t",
        token,
        "--interval",
        &interval_secs.to_string(),
    ]);
    if let Some(t) = timeout_secs {
        cmd.args(["--timeout", &t.to_string()]);
    }
    if let Some(s) = socket {
        cmd.args(["--socket", s]);
    }
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run `room watch`: {e}"))?;

    if output.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if detect_token_expiry(&stderr) {
            Err(stderr.to_string())
        } else {
            // Non-zero exit without token error — likely timeout or interrupt
            Ok(false)
        }
    }
}

/// Check if a response suggests the auth token is invalid/expired.
///
/// Matches error messages from both the broker (`invalid_token`) and the
/// oneshot token resolver (`token not recognised`).
pub fn detect_token_expiry(response: &str) -> bool {
    let lower = response.to_lowercase();
    (lower.contains("invalid") && lower.contains("token"))
        || lower.contains("unauthorized")
        || (lower.contains("token") && lower.contains("expired"))
        || (lower.contains("token") && lower.contains("not recognised"))
}

/// Extract the token UUID from room join JSON output.
fn extract_token(json_str: &str) -> Result<String, String> {
    // Try parsing each line (room may output extra lines)
    for line in json_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(token) = v.get("token").and_then(|t| t.as_str()) {
                    return Ok(token.to_string());
                }
            }
        }
    }
    Err(format!("no token found in join output: {json_str}"))
}

/// Read a cached token from the token file.
fn read_cached_token(path: &PathBuf) -> Result<String, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read token file {}: {e}", path.display()))?;
    extract_token(&content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mutex to serialize env-var tests (env is process-global state).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Clear ROOM_TOKEN to avoid cross-test contamination.
    fn clear_room_token_env() {
        unsafe { std::env::remove_var(ROOM_TOKEN_ENV) };
    }

    #[test]
    fn join_room_uses_room_token_env_when_set() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_room_token_env();

        unsafe { std::env::set_var(ROOM_TOKEN_ENV, "pre-provisioned-uuid-123") };
        let result = join_room("test-room", "test-agent", None).unwrap();
        assert_eq!(result.token, "pre-provisioned-uuid-123");
        assert_eq!(result.username, "test-agent");

        clear_room_token_env();
    }

    #[test]
    fn join_room_ignores_empty_room_token_env() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_room_token_env();

        unsafe { std::env::set_var(ROOM_TOKEN_ENV, "") };
        // With empty ROOM_TOKEN, join_room falls through to `room join` with a
        // non-existent socket so it fails. We verify it doesn't return the empty token.
        let result = join_room("test-room", "test-agent", Some("/nonexistent/socket.sock"));
        // Should error because the socket doesn't exist
        assert!(result.is_err());

        clear_room_token_env();
    }

    #[test]
    fn room_token_env_constant_value() {
        assert_eq!(ROOM_TOKEN_ENV, "ROOM_TOKEN");
    }

    #[test]
    fn extract_token_from_valid_json() {
        let json = r#"{"type":"token","token":"abc-123","username":"saphire"}"#;
        assert_eq!(extract_token(json).unwrap(), "abc-123");
    }

    #[test]
    fn extract_token_from_multiline() {
        let output =
            "some debug line\n{\"type\":\"token\",\"token\":\"def-456\",\"username\":\"x\"}\n";
        assert_eq!(extract_token(output).unwrap(), "def-456");
    }

    #[test]
    fn extract_token_fails_on_garbage() {
        assert!(extract_token("not json at all").is_err());
    }

    #[test]
    fn detect_token_expiry_patterns() {
        assert!(detect_token_expiry("error: invalid token"));
        assert!(detect_token_expiry("Unauthorized"));
        assert!(detect_token_expiry("token expired"));
        assert!(detect_token_expiry("Token is invalid"));
        assert!(detect_token_expiry(
            "token not recognised — run: room join myroom <username>"
        ));
        assert!(!detect_token_expiry("all good"));
        assert!(!detect_token_expiry("valid response"));
    }

    #[test]
    fn set_status_message_with_status_text() {
        assert_eq!(
            build_set_status_message("reading src/broker.rs"),
            "/set_status reading src/broker.rs"
        );
    }

    #[test]
    fn set_status_message_empty_clears_status() {
        assert_eq!(build_set_status_message(""), "/set_status");
    }

    #[test]
    fn log_file_path_format() {
        assert_eq!(
            log_file_path("saphire"),
            PathBuf::from("/tmp/ralph-room-saphire.log")
        );
    }

    #[test]
    fn token_file_path_format() {
        assert_eq!(
            token_file_path("agent1"),
            PathBuf::from("/tmp/room-agent1.token")
        );
    }

    #[test]
    fn legacy_token_file_path_format() {
        assert_eq!(
            legacy_token_file_path("myroom", "agent1"),
            PathBuf::from("/tmp/room-myroom-agent1.token")
        );
    }

    #[test]
    fn is_username_taken_detects_already_in_use() {
        assert!(is_username_taken(
            "Error: username 'c3po' is already in use in this room"
        ));
        assert!(is_username_taken(
            "error: username 'agent' is already in use"
        ));
        assert!(is_username_taken("USERNAME ALREADY IN USE"));
    }

    #[test]
    fn is_username_taken_detects_already_taken() {
        assert!(is_username_taken("username already taken"));
        assert!(is_username_taken("Username is already taken in this room"));
    }

    #[test]
    fn is_username_taken_rejects_unrelated_errors() {
        assert!(!is_username_taken("connection refused"));
        assert!(!is_username_taken("room not found"));
        assert!(!is_username_taken("invalid token"));
        assert!(!is_username_taken("timeout"));
        // Partial matches should not trigger
        assert!(!is_username_taken("already connected"));
        assert!(!is_username_taken("username invalid"));
    }

    #[test]
    fn watch_room_nonexistent_socket_returns_error() {
        let result = watch_room(
            "test-room",
            "bad-token",
            1,
            Some(1),
            Some("/nonexistent.sock"),
        );
        // Should fail to connect — either Err or Ok(false) depending on how room handles it
        assert!(
            result.is_err() || result == Ok(false),
            "watch with bad socket should fail or return false"
        );
    }
}
