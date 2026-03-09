use std::path::PathBuf;
use std::process::Command;

use room_protocol::Message;

/// Returns the path to the ralph log file.
pub fn log_file_path(username: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/ralph-room-{username}.log"))
}

/// Returns the path to the room token file.
pub fn token_file_path(room_id: &str, username: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/room-{room_id}-{username}.token"))
}

/// Join a room and return the token UUID.
///
/// Runs `room join <room_id> <username>` and parses the token from JSON output.
/// Falls back to cached token file if join fails.
/// If `socket` is provided, passes `--socket <path>` to the `room` command.
pub fn join_room(room_id: &str, username: &str, socket: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("room");
    cmd.args(["join", room_id, username]);
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
        // Fall back to cached token file
        let token_file = token_file_path(room_id, username);
        tracing::warn!(
            "room join failed, trying cached token at {}",
            token_file.display()
        );
        read_cached_token(&token_file)
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
            token_file_path("myroom", "agent1"),
            PathBuf::from("/tmp/room-myroom-agent1.token")
        );
    }
}
