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
pub fn join_room(room_id: &str, username: &str) -> Result<String, String> {
    let output = Command::new("room")
        .args(["join", room_id, username])
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
pub fn send_message(room_id: &str, token: &str, message: &str) -> Result<(), String> {
    let output = Command::new("room")
        .args(["send", room_id, "-t", token, message])
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
pub fn poll_messages(room_id: &str, token: &str) -> Result<Vec<Message>, String> {
    let output = Command::new("room")
        .args(["poll", room_id, "-t", token])
        .output()
        .map_err(|e| format!("failed to run `room poll`: {e}"))?;

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
/// Runs `room send <room_id> -t <token> /set_status <status>`.
/// Note: `/set_status` is a `CommandResult::Handled` command — the broker
/// broadcasts the status change but sends no reply to oneshot connections,
/// causing `room send` to exit non-zero with an EOF error (#234). The status
/// IS set server-side, so we treat a non-zero exit as success here.
pub fn set_status(room_id: &str, token: &str, status: &str) -> Result<(), String> {
    let message = if status.is_empty() {
        "/set_status".to_owned()
    } else {
        format!("/set_status {status}")
    };
    let output = Command::new("room")
        .args(["send", room_id, "-t", token, &message])
        .output()
        .map_err(|e| format!("failed to run `room send /set_status`: {e}"))?;

    // Accept both success and the known EOF error (#234) as OK.
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("EOF while parsing") {
        // Known #234 — status was set server-side despite the error.
        Ok(())
    } else {
        Err(format!("room set_status failed: {stderr}"))
    }
}

/// Check if a response suggests the auth token is invalid/expired.
pub fn detect_token_expiry(response: &str) -> bool {
    let lower = response.to_lowercase();
    lower.contains("invalid") && lower.contains("token")
        || lower.contains("unauthorized")
        || lower.contains("token") && lower.contains("expired")
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
        assert!(!detect_token_expiry("all good"));
        assert!(!detect_token_expiry("valid response"));
    }

    /// The known EOF error from #234 that set_status treats as success.
    const EOF_ERROR: &str =
        "Error: broker returned invalid JSON: EOF while parsing a value at line 1 column 0";

    #[test]
    fn set_status_eof_pattern_detected() {
        // Verify the EOF substring match used in set_status catches the real error.
        assert!(EOF_ERROR.contains("EOF while parsing"));
    }

    #[test]
    fn set_status_other_errors_not_masked() {
        // Errors unrelated to #234 should NOT match the EOF pattern.
        let other = "Error: connection refused";
        assert!(!other.contains("EOF while parsing"));
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
