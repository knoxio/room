use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

/// Predefined tool profiles for different agent roles.
///
/// Each profile defines a set of auto-approved (allowed) and hard-blocked
/// (disallowed) tools appropriate for that role. Explicit `--allow-tools`
/// and `--disallow-tools` flags merge on top of the profile's base lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Full dev access — read, write, edit, search, build, test, git, room.
    Coder,
    /// PR review — read-only code access, room + gh pr commands. No writes.
    Reviewer,
    /// BA/coordination role — read + room + gh commands. No code writes.
    Coordinator,
    /// Notion management — read project, manage Notion, room + gh. No code writes.
    Notion,
    /// Read-only analysis — search and read only. No shell, no writes.
    Reader,
}

impl Profile {
    /// Tools auto-approved for this profile.
    pub fn allowed_tools(&self) -> Vec<&'static str> {
        match self {
            Profile::Coder => vec![
                "Read",
                "Edit",
                "Write",
                "Glob",
                "Grep",
                "WebSearch",
                "Bash(room *)",
                "Bash(git *)",
                "Bash(cargo *)",
                "Bash(gh *)",
                "Bash(bash scripts/pre-push.sh)",
            ],
            Profile::Reviewer => vec!["Read", "Glob", "Grep", "Bash(room *)", "Bash(gh pr *)"],
            Profile::Coordinator => vec!["Read", "Glob", "Grep", "Bash(room *)", "Bash(gh *)"],
            Profile::Notion => vec![
                "Read",
                "Glob",
                "Grep",
                "Bash(room *)",
                "Bash(gh *)",
                "mcp__notion__*",
            ],
            Profile::Reader => vec!["Read", "Glob", "Grep"],
        }
    }

    /// Tools hard-blocked for this profile.
    pub fn disallowed_tools(&self) -> Vec<&'static str> {
        match self {
            Profile::Coder => vec![],
            Profile::Reviewer => vec!["Write", "Edit"],
            Profile::Coordinator => vec!["Write", "Edit"],
            Profile::Notion => vec!["Write", "Edit", "Bash(git push *)", "Bash(git commit *)"],
            Profile::Reader => vec!["Bash", "Write", "Edit"],
        }
    }

    /// All known profile names, for error messages.
    pub const NAMES: &[&str] = &["coder", "reviewer", "coordinator", "notion", "reader"];
}

impl FromStr for Profile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "coder" => Ok(Profile::Coder),
            "reviewer" => Ok(Profile::Reviewer),
            "coordinator" => Ok(Profile::Coordinator),
            "notion" => Ok(Profile::Notion),
            "reader" => Ok(Profile::Reader),
            other => Err(format!(
                "unknown profile '{}'. valid profiles: {}",
                other,
                Profile::NAMES.join(", ")
            )),
        }
    }
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Profile::Coder => "coder",
            Profile::Reviewer => "reviewer",
            Profile::Coordinator => "coordinator",
            Profile::Notion => "notion",
            Profile::Reader => "reader",
        };
        f.write_str(name)
    }
}

/// Merge a profile's base tool lists with explicit CLI overrides.
///
/// - `--allow-tools` values are appended to (not replacing) the profile's
///   allowed list. Duplicates are removed.
/// - `--disallow-tools` values are appended to the profile's disallowed list.
///   Duplicates are removed.
/// - If no profile is set, returns the CLI values as-is (falls through to
///   existing `resolve_allowed_tools` / `resolve_disallowed_tools` logic).
pub fn merge_profile_with_overrides(
    profile: Option<Profile>,
    cli_allow: &[String],
    cli_disallow: &[String],
) -> (Vec<String>, Vec<String>) {
    let Some(profile) = profile else {
        return (cli_allow.to_vec(), cli_disallow.to_vec());
    };

    let mut allowed: Vec<String> = profile
        .allowed_tools()
        .into_iter()
        .map(String::from)
        .collect();
    for tool in cli_allow {
        if !allowed.iter().any(|t| t == tool) {
            allowed.push(tool.clone());
        }
    }

    let mut disallowed: Vec<String> = profile
        .disallowed_tools()
        .into_iter()
        .map(String::from)
        .collect();
    for tool in cli_disallow {
        if !disallowed.iter().any(|t| t == tool) {
            disallowed.push(tool.clone());
        }
    }

    (allowed, disallowed)
}

/// Safe default tools that ralph passes to claude when no explicit
/// --allow-tools flag or RALPH_ALLOWED_TOOLS env var is set.
///
/// In `-p` mode, tools not listed here are auto-denied (no terminal for
/// approval prompts). This list must include all tools an agent needs for
/// typical coding workflows: reading, editing, writing files, searching,
/// running builds/tests, and communicating via room.
pub const DEFAULT_ALLOWED_TOOLS: &[&str] = &[
    "Read",
    "Edit",
    "Write",
    "Glob",
    "Grep",
    "WebSearch",
    "Bash(room *)",
    "Bash(git *)",
    "Bash(cargo *)",
    "Bash(gh *)",
    "Bash(bash scripts/pre-push.sh)",
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
    // Pass the pre-provisioned token so claude's CLAUDE.md instructions
    // can skip `room join` and use the token directly.
    if let Ok(token) = std::env::var(crate::room::ROOM_TOKEN_ENV) {
        if !token.is_empty() {
            cmd.env(crate::room::ROOM_TOKEN_ENV, &token);
        }
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
        assert!(result.contains(&"Edit".to_string()));
        assert!(result.contains(&"Write".to_string()));
        assert!(result.contains(&"Glob".to_string()));
        assert!(result.contains(&"Bash(room *)".to_string()));
        assert!(result.contains(&"Bash(cargo *)".to_string()));
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

    // --- Profile tests ---

    #[test]
    fn profile_parse_all_variants() {
        assert_eq!("coder".parse::<Profile>().unwrap(), Profile::Coder);
        assert_eq!("reviewer".parse::<Profile>().unwrap(), Profile::Reviewer);
        assert_eq!(
            "coordinator".parse::<Profile>().unwrap(),
            Profile::Coordinator
        );
        assert_eq!("notion".parse::<Profile>().unwrap(), Profile::Notion);
        assert_eq!("reader".parse::<Profile>().unwrap(), Profile::Reader);
    }

    #[test]
    fn profile_parse_case_insensitive() {
        assert_eq!("CODER".parse::<Profile>().unwrap(), Profile::Coder);
        assert_eq!("Reviewer".parse::<Profile>().unwrap(), Profile::Reviewer);
        assert_eq!("NOTION".parse::<Profile>().unwrap(), Profile::Notion);
    }

    #[test]
    fn profile_parse_invalid() {
        let err = "unknown".parse::<Profile>().unwrap_err();
        assert!(err.contains("unknown profile"));
        assert!(err.contains("coder"));
    }

    #[test]
    fn profile_display_roundtrip() {
        for name in Profile::NAMES {
            let profile: Profile = name.parse().unwrap();
            assert_eq!(profile.to_string(), *name);
        }
    }

    #[test]
    fn profile_coder_has_full_access() {
        let allowed = Profile::Coder.allowed_tools();
        assert!(allowed.contains(&"Read"));
        assert!(allowed.contains(&"Write"));
        assert!(allowed.contains(&"Edit"));
        assert!(allowed.contains(&"Bash(cargo *)"));
        assert!(Profile::Coder.disallowed_tools().is_empty());
    }

    #[test]
    fn profile_reviewer_blocks_writes() {
        let allowed = Profile::Reviewer.allowed_tools();
        assert!(allowed.contains(&"Read"));
        assert!(allowed.contains(&"Bash(gh pr *)"));
        assert!(!allowed.contains(&"Write"));
        assert!(!allowed.contains(&"Edit"));
        let disallowed = Profile::Reviewer.disallowed_tools();
        assert!(disallowed.contains(&"Write"));
        assert!(disallowed.contains(&"Edit"));
    }

    #[test]
    fn profile_coordinator_blocks_writes() {
        let allowed = Profile::Coordinator.allowed_tools();
        assert!(allowed.contains(&"Bash(gh *)"));
        assert!(!allowed.contains(&"Write"));
        let disallowed = Profile::Coordinator.disallowed_tools();
        assert!(disallowed.contains(&"Write"));
        assert!(disallowed.contains(&"Edit"));
    }

    #[test]
    fn profile_notion_has_notion_tools_and_blocks_push() {
        let allowed = Profile::Notion.allowed_tools();
        assert!(allowed.contains(&"mcp__notion__*"));
        assert!(allowed.contains(&"Read"));
        assert!(!allowed.contains(&"Write"));
        let disallowed = Profile::Notion.disallowed_tools();
        assert!(disallowed.contains(&"Write"));
        assert!(disallowed.contains(&"Bash(git push *)"));
        assert!(disallowed.contains(&"Bash(git commit *)"));
    }

    #[test]
    fn profile_reader_is_minimal() {
        let allowed = Profile::Reader.allowed_tools();
        assert_eq!(allowed, vec!["Read", "Glob", "Grep"]);
        let disallowed = Profile::Reader.disallowed_tools();
        assert!(disallowed.contains(&"Bash"));
        assert!(disallowed.contains(&"Write"));
        assert!(disallowed.contains(&"Edit"));
    }

    #[test]
    fn merge_no_profile_passes_through() {
        let allow = vec!["Read".to_string()];
        let disallow = vec!["Write".to_string()];
        let (a, d) = merge_profile_with_overrides(None, &allow, &disallow);
        assert_eq!(a, vec!["Read"]);
        assert_eq!(d, vec!["Write"]);
    }

    #[test]
    fn merge_profile_only_no_overrides() {
        let (a, d) = merge_profile_with_overrides(Some(Profile::Reviewer), &[], &[]);
        assert_eq!(
            a,
            Profile::Reviewer
                .allowed_tools()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            d,
            Profile::Reviewer
                .disallowed_tools()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn merge_profile_with_extra_allow() {
        let extra = vec!["Bash(cargo test)".to_string()];
        let (a, _) = merge_profile_with_overrides(Some(Profile::Reviewer), &extra, &[]);
        assert!(a.contains(&"Bash(cargo test)".to_string()));
        assert!(a.contains(&"Read".to_string())); // from profile
    }

    #[test]
    fn merge_profile_deduplicates_allow() {
        let extra = vec!["Read".to_string()]; // already in reviewer profile
        let (a, _) = merge_profile_with_overrides(Some(Profile::Reviewer), &extra, &[]);
        assert_eq!(a.iter().filter(|t| *t == "Read").count(), 1);
    }

    #[test]
    fn merge_profile_with_extra_disallow() {
        let extra = vec!["Bash(rm *)".to_string()];
        let (_, d) = merge_profile_with_overrides(Some(Profile::Coder), &[], &extra);
        assert_eq!(d, vec!["Bash(rm *)"]);
    }

    #[test]
    fn merge_profile_deduplicates_disallow() {
        let extra = vec!["Write".to_string()]; // already in reviewer profile
        let (_, d) = merge_profile_with_overrides(Some(Profile::Reviewer), &[], &extra);
        assert_eq!(d.iter().filter(|t| *t == "Write").count(), 1);
    }

    #[test]
    fn merge_profile_both_overrides() {
        let allow = vec!["WebFetch".to_string()];
        let disallow = vec!["Bash(rm *)".to_string()];
        let (a, d) = merge_profile_with_overrides(Some(Profile::Notion), &allow, &disallow);
        assert!(a.contains(&"WebFetch".to_string()));
        assert!(a.contains(&"mcp__notion__*".to_string()));
        assert!(d.contains(&"Bash(rm *)".to_string()));
        assert!(d.contains(&"Write".to_string()));
    }
}
