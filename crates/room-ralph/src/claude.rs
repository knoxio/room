use std::path::{Path, PathBuf};
use std::process::Command;

/// Safe default tools that ralph passes to claude when no explicit
/// --allow-tools flag or RALPH_ALLOWED_TOOLS env var is set.
///
/// These control auto-approval — tools not listed may still be available
/// but require user approval (which auto-denies in `-p` mode for most tools).
pub const DEFAULT_ALLOWED_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "WebSearch",
    "Bash(room *)",
    "Bash(git status)",
    "Bash(git log)",
    "Bash(git diff)",
];

/// Default disallowed tools — hard-blocked from the session entirely.
///
/// Empty by default (no tools blocked). Users can add restrictions via
/// `--disallow-tools` or `RALPH_DISALLOWED_TOOLS` env var.
pub const DEFAULT_DISALLOWED_TOOLS: &[&str] = &[];

/// Resolve the effective allowed-tools list.
///
/// Precedence: CLI --allow-tools > RALPH_ALLOWED_TOOLS env > defaults.
/// Special value "none" (case-insensitive) disables tool restrictions entirely.
pub fn resolve_allowed_tools(cli_tools: &[String]) -> Vec<String> {
    // CLI takes highest precedence
    if !cli_tools.is_empty() {
        if cli_tools.len() == 1 && cli_tools[0].eq_ignore_ascii_case("none") {
            return Vec::new();
        }
        return cli_tools.to_vec();
    }

    // Check env var
    if let Ok(env_val) = std::env::var("RALPH_ALLOWED_TOOLS") {
        let trimmed = env_val.trim();
        if trimmed.eq_ignore_ascii_case("none") {
            return Vec::new();
        }
        if !trimmed.is_empty() {
            return trimmed.split(',').map(|s| s.trim().to_string()).collect();
        }
    }

    // Fall back to defaults
    DEFAULT_ALLOWED_TOOLS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// Resolve the effective disallowed-tools list.
///
/// Precedence: CLI --disallow-tools > RALPH_DISALLOWED_TOOLS env > defaults.
/// Special value "none" (case-insensitive) clears all disallowed tools.
pub fn resolve_disallowed_tools(cli_tools: &[String]) -> Vec<String> {
    // CLI takes highest precedence
    if !cli_tools.is_empty() {
        if cli_tools.len() == 1 && cli_tools[0].eq_ignore_ascii_case("none") {
            return Vec::new();
        }
        return cli_tools.to_vec();
    }

    // Check env var (clap handles this for the CLI flag, but we keep the
    // manual check for consistency with resolve_allowed_tools and to support
    // the "none" sentinel when set via env)
    if let Ok(env_val) = std::env::var("RALPH_DISALLOWED_TOOLS") {
        let trimmed = env_val.trim();
        if trimmed.eq_ignore_ascii_case("none") {
            return Vec::new();
        }
        if !trimmed.is_empty() {
            return trimmed.split(',').map(|s| s.trim().to_string()).collect();
        }
    }

    // Fall back to defaults (empty)
    DEFAULT_DISALLOWED_TOOLS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// Output from a claude -p invocation.
pub struct ClaudeOutput {
    /// Raw JSON output from claude --output-format json
    pub raw_json: String,
    /// Process exit code
    pub exit_code: i32,
}

/// Environment variables to strip from the child claude process.
///
/// Claude Code sets these to detect (and reject) nested sessions. Since
/// room-ralph intentionally spawns independent `claude -p` processes,
/// the nesting guard must be bypassed.
const STRIPPED_ENV_VARS: &[&str] = &["CLAUDECODE", "CLAUDE_CODE_ENTRY_POINT"];

/// Build the `claude` command with all flags, ready to spawn.
fn build_claude_command(
    model: &str,
    add_dirs: &[PathBuf],
    allowed_tools: &[String],
    disallowed_tools: &[String],
) -> Command {
    let mut cmd = Command::new("claude");
    for var in STRIPPED_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd.args(["-p", "--model", model, "--output-format", "json"]);
    for dir in add_dirs {
        cmd.args(["--add-dir", &dir.display().to_string()]);
    }
    for tool in allowed_tools {
        cmd.args(["--allowedTools", tool]);
    }
    for tool in disallowed_tools {
        cmd.args(["--disallowedTools", tool]);
    }
    cmd
}

/// Spawn `claude -p` with the given prompt file and return its output.
///
/// Reads the prompt file and pipes it to claude via stdin.
/// Uses `--output-format json` for structured output.
pub fn spawn_claude(
    model: &str,
    prompt_file: &Path,
    add_dirs: &[PathBuf],
    allowed_tools: &[String],
    disallowed_tools: &[String],
) -> Result<ClaudeOutput, String> {
    let prompt = std::fs::read_to_string(prompt_file)
        .map_err(|e| format!("cannot read prompt file {}: {e}", prompt_file.display()))?;

    let mut cmd = build_claude_command(model, add_dirs, allowed_tools, disallowed_tools);
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
    use std::sync::Mutex;

    /// Mutex to serialize tests that touch RALPH_ALLOWED_TOOLS (env is process-global).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    #[test]
    fn build_command_base_args() {
        let cmd = build_claude_command("opus", &[], &[], &[]);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, ["-p", "--model", "opus", "--output-format", "json"]);
    }

    #[test]
    fn build_command_with_add_dirs() {
        let dirs = vec![PathBuf::from("/tmp/dir1"), PathBuf::from("/tmp/dir2")];
        let cmd = build_claude_command("sonnet", &dirs, &[], &[]);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(args.contains(&"--add-dir".to_string()));
        assert!(args.contains(&"/tmp/dir1".to_string()));
        assert!(args.contains(&"/tmp/dir2".to_string()));
    }

    #[test]
    fn build_command_with_allowed_tools() {
        let tools = vec!["Bash".to_string(), "Read".to_string(), "Write".to_string()];
        let cmd = build_claude_command("opus", &[], &tools, &[]);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        // Each tool should be preceded by --allowedTools
        let tool_flags: Vec<_> = args
            .windows(2)
            .filter(|w| w[0] == "--allowedTools")
            .map(|w| w[1].clone())
            .collect();
        assert_eq!(tool_flags, ["Bash", "Read", "Write"]);
    }

    #[test]
    fn build_command_empty_allowed_tools() {
        let cmd = build_claude_command("opus", &[], &[], &[]);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(!args.contains(&"--allowedTools".to_string()));
    }

    #[test]
    fn build_command_with_dirs_and_tools() {
        let dirs = vec![PathBuf::from("/tmp/work")];
        let tools = vec!["Bash".to_string()];
        let cmd = build_claude_command("opus", &dirs, &tools, &[]);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            args,
            [
                "-p",
                "--model",
                "opus",
                "--output-format",
                "json",
                "--add-dir",
                "/tmp/work",
                "--allowedTools",
                "Bash"
            ]
        );
    }

    #[test]
    fn resolve_defaults_when_empty() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
        let result = resolve_allowed_tools(&[]);
        assert_eq!(result.len(), DEFAULT_ALLOWED_TOOLS.len());
        assert!(result.contains(&"Read".to_string()));
        assert!(result.contains(&"Glob".to_string()));
        assert!(result.contains(&"Bash(room *)".to_string()));
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
    }

    #[test]
    fn resolve_cli_overrides_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
        let cli = vec!["Write".to_string(), "Edit".to_string()];
        let result = resolve_allowed_tools(&cli);
        assert_eq!(result, vec!["Write", "Edit"]);
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
    }

    #[test]
    fn resolve_cli_none_disables_restrictions() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
        let cli = vec!["none".to_string()];
        let result = resolve_allowed_tools(&cli);
        assert!(result.is_empty());
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
    }

    #[test]
    fn resolve_cli_none_case_insensitive() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
        let cli = vec!["NONE".to_string()];
        let result = resolve_allowed_tools(&cli);
        assert!(result.is_empty());

        let cli = vec!["None".to_string()];
        let result = resolve_allowed_tools(&cli);
        assert!(result.is_empty());
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
    }

    #[test]
    fn resolve_env_overrides_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
        std::env::set_var("RALPH_ALLOWED_TOOLS", "Bash,Read,WebSearch");
        let result = resolve_allowed_tools(&[]);
        assert_eq!(result, vec!["Bash", "Read", "WebSearch"]);
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
    }

    #[test]
    fn resolve_env_none_disables_restrictions() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
        std::env::set_var("RALPH_ALLOWED_TOOLS", "none");
        let result = resolve_allowed_tools(&[]);
        assert!(result.is_empty());
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
    }

    #[test]
    fn resolve_cli_takes_precedence_over_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
        std::env::set_var("RALPH_ALLOWED_TOOLS", "Bash,Read");
        let cli = vec!["Write".to_string()];
        let result = resolve_allowed_tools(&cli);
        assert_eq!(result, vec!["Write"]);
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
    }

    #[test]
    fn resolve_env_trims_whitespace() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
        std::env::set_var("RALPH_ALLOWED_TOOLS", " Bash , Read , Grep ");
        let result = resolve_allowed_tools(&[]);
        assert_eq!(result, vec!["Bash", "Read", "Grep"]);
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
    }

    #[test]
    fn resolve_env_empty_string_uses_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
        std::env::set_var("RALPH_ALLOWED_TOOLS", "");
        let result = resolve_allowed_tools(&[]);
        assert_eq!(result.len(), DEFAULT_ALLOWED_TOOLS.len());
        std::env::remove_var("RALPH_ALLOWED_TOOLS");
    }

    #[test]
    fn build_command_strips_claudecode_env_vars() {
        let cmd = build_claude_command("opus", &[], &[], &[]);
        let removals: Vec<_> = cmd
            .get_envs()
            .filter(|(_, val)| val.is_none())
            .map(|(key, _)| key.to_string_lossy().to_string())
            .collect();
        for var in STRIPPED_ENV_VARS {
            assert!(
                removals.contains(&var.to_string()),
                "{var} should be removed from child env"
            );
        }
    }

    #[test]
    fn stripped_env_vars_contains_expected_entries() {
        assert!(STRIPPED_ENV_VARS.contains(&"CLAUDECODE"));
        assert!(STRIPPED_ENV_VARS.contains(&"CLAUDE_CODE_ENTRY_POINT"));
    }

    // --- disallowed tools tests ---

    #[test]
    fn build_command_with_disallowed_tools() {
        let disallowed = vec![
            "Write".to_string(),
            "Edit".to_string(),
            "Bash(python3:*)".to_string(),
        ];
        let cmd = build_claude_command("opus", &[], &[], &disallowed);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let disallowed_flags: Vec<_> = args
            .windows(2)
            .filter(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .collect();
        assert_eq!(disallowed_flags, ["Write", "Edit", "Bash(python3:*)"]);
    }

    #[test]
    fn build_command_empty_disallowed_tools() {
        let cmd = build_claude_command("opus", &[], &[], &[]);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(!args.contains(&"--disallowedTools".to_string()));
    }

    #[test]
    fn build_command_both_allowed_and_disallowed() {
        let allowed = vec!["Read".to_string(), "Glob".to_string()];
        let disallowed = vec!["Write".to_string(), "Edit".to_string()];
        let cmd = build_claude_command("opus", &[], &allowed, &disallowed);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let allow_flags: Vec<_> = args
            .windows(2)
            .filter(|w| w[0] == "--allowedTools")
            .map(|w| w[1].clone())
            .collect();
        let disallow_flags: Vec<_> = args
            .windows(2)
            .filter(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .collect();
        assert_eq!(allow_flags, ["Read", "Glob"]);
        assert_eq!(disallow_flags, ["Write", "Edit"]);
    }

    #[test]
    fn resolve_disallowed_defaults_empty() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
        let result = resolve_disallowed_tools(&[]);
        assert!(result.is_empty());
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
    }

    #[test]
    fn resolve_disallowed_cli_overrides() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
        let cli = vec!["Write".to_string(), "Edit".to_string()];
        let result = resolve_disallowed_tools(&cli);
        assert_eq!(result, vec!["Write", "Edit"]);
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
    }

    #[test]
    fn resolve_disallowed_cli_none_clears() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
        let cli = vec!["none".to_string()];
        let result = resolve_disallowed_tools(&cli);
        assert!(result.is_empty());
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
    }

    #[test]
    fn resolve_disallowed_env_overrides() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
        std::env::set_var("RALPH_DISALLOWED_TOOLS", "Bash,Write");
        let result = resolve_disallowed_tools(&[]);
        assert_eq!(result, vec!["Bash", "Write"]);
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
    }

    #[test]
    fn resolve_disallowed_cli_takes_precedence_over_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
        std::env::set_var("RALPH_DISALLOWED_TOOLS", "Bash,Write");
        let cli = vec!["Edit".to_string()];
        let result = resolve_disallowed_tools(&cli);
        assert_eq!(result, vec!["Edit"]);
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
    }

    #[test]
    fn resolve_disallowed_env_none_clears() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
        std::env::set_var("RALPH_DISALLOWED_TOOLS", "none");
        let result = resolve_disallowed_tools(&[]);
        assert!(result.is_empty());
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
    }

    #[test]
    fn resolve_disallowed_env_trims_whitespace() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
        std::env::set_var("RALPH_DISALLOWED_TOOLS", " Write , Edit , Bash(rm:*) ");
        let result = resolve_disallowed_tools(&[]);
        assert_eq!(result, vec!["Write", "Edit", "Bash(rm:*)"]);
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
    }

    #[test]
    fn resolve_disallowed_env_empty_uses_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
        std::env::set_var("RALPH_DISALLOWED_TOOLS", "");
        let result = resolve_disallowed_tools(&[]);
        assert!(result.is_empty()); // defaults are empty
        std::env::remove_var("RALPH_DISALLOWED_TOOLS");
    }
}
