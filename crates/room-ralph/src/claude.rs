use std::path::{Path, PathBuf};
use std::process::Command;

/// Output from a claude -p invocation.
pub struct ClaudeOutput {
    /// Raw JSON output from claude --output-format json
    pub raw_json: String,
    /// Process exit code
    pub exit_code: i32,
}

/// Spawn `claude -p` with the given prompt file and return its output.
///
/// Writes the prompt to a temp file and pipes it to claude via stdin.
/// Uses `--output-format json` for structured output.
pub fn spawn_claude(
    model: &str,
    prompt_file: &Path,
    add_dirs: &[PathBuf],
) -> Result<ClaudeOutput, String> {
    let prompt = std::fs::read_to_string(prompt_file)
        .map_err(|e| format!("cannot read prompt file {}: {e}", prompt_file.display()))?;

    let mut cmd = Command::new("claude");
    cmd.args(["-p", "--model", model, "--output-format", "json"]);
    for dir in add_dirs {
        cmd.args(["--add-dir", &dir.display().to_string()]);
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn claude: {e}"))?;

    // Write prompt to stdin
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(prompt.as_bytes())
            .map_err(|e| format!("failed to write prompt to claude stdin: {e}"))?;
        // stdin is dropped here, closing the pipe
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for claude: {e}"))?;

    let exit_code = output.status.code().unwrap_or(-1);
    let raw_json = String::from_utf8_lossy(&output.stdout).to_string();

    Ok(ClaudeOutput {
        raw_json,
        exit_code,
    })
}

/// Extract the text response from claude's JSON output.
///
/// Tries .result, .content, .error in order. Returns "no output" if none found.
pub fn extract_response(json_output: &str) -> String {
    if json_output.trim().is_empty() {
        return "no output".to_string();
    }

    match serde_json::from_str::<serde_json::Value>(json_output) {
        Ok(v) => {
            if let Some(s) = v.get("result").and_then(|v| v.as_str()) {
                return s.to_string();
            }
            if let Some(s) = v.get("content").and_then(|v| v.as_str()) {
                return s.to_string();
            }
            if let Some(s) = v.get("error").and_then(|v| v.as_str()) {
                return s.to_string();
            }
            "no output".to_string()
        }
        Err(_) => {
            // Not JSON — return raw text (truncated)
            let truncated: String = json_output.chars().take(2000).collect();
            truncated
        }
    }
}

/// Check if claude's output/exit code suggests context window exhaustion.
pub fn detect_context_exhaustion(exit_code: i32, response: &str) -> bool {
    if exit_code == 0 {
        return false;
    }
    let lower = response.to_lowercase();
    lower.contains("context") && lower.contains("limit")
        || lower.contains("context") && lower.contains("window")
        || lower.contains("context") && lower.contains("exhaust")
        || lower.contains("token") && lower.contains("limit")
        || lower.contains("conversation") && lower.contains("too") && lower.contains("long")
        || lower.contains("maximum") && lower.contains("context")
        || lower.contains("context") && lower.contains("length")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_response_from_result_field() {
        let json = r#"{"result":"hello world"}"#;
        assert_eq!(extract_response(json), "hello world");
    }

    #[test]
    fn extract_response_from_content_field() {
        let json = r#"{"content":"from content"}"#;
        assert_eq!(extract_response(json), "from content");
    }

    #[test]
    fn extract_response_from_error_field() {
        let json = r#"{"error":"something broke"}"#;
        assert_eq!(extract_response(json), "something broke");
    }

    #[test]
    fn extract_response_empty_input() {
        assert_eq!(extract_response(""), "no output");
        assert_eq!(extract_response("  "), "no output");
    }

    #[test]
    fn extract_response_non_json() {
        assert_eq!(extract_response("plain text output"), "plain text output");
    }

    #[test]
    fn extract_response_json_without_known_fields() {
        let json = r#"{"unknown":"field"}"#;
        assert_eq!(extract_response(json), "no output");
    }

    #[test]
    fn detect_context_exhaustion_exit_zero() {
        assert!(!detect_context_exhaustion(0, "context limit reached"));
    }

    #[test]
    fn detect_context_exhaustion_patterns() {
        assert!(detect_context_exhaustion(1, "context limit exceeded"));
        assert!(detect_context_exhaustion(1, "context window full"));
        assert!(detect_context_exhaustion(1, "context exhausted"));
        assert!(detect_context_exhaustion(1, "token limit reached"));
        assert!(detect_context_exhaustion(1, "conversation too long"));
        assert!(detect_context_exhaustion(1, "maximum context reached"));
        assert!(detect_context_exhaustion(1, "context length exceeded"));
    }

    #[test]
    fn detect_context_exhaustion_false_on_unrelated() {
        assert!(!detect_context_exhaustion(1, "syntax error"));
        assert!(!detect_context_exhaustion(1, "network timeout"));
        assert!(!detect_context_exhaustion(1, ""));
    }
}
