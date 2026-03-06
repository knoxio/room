use std::path::PathBuf;

use clap::Parser;

pub mod claude;
pub mod loop_runner;
pub mod monitor;
pub mod progress;
pub mod prompt;
pub mod room;

/// Autonomous agent wrapper for room — runs `claude -p` with auto-restart
/// on context exhaustion.
///
/// Implements the "ralph loop" pattern: spawns fresh `claude -p` instances
/// in a loop, feeding room context and progress files on each restart.
/// Context exhaustion is not task death — progress persists in files.
#[derive(Parser, Debug)]
#[command(name = "room-ralph", version, about)]
pub struct Cli {
    /// Room ID to join
    #[arg(env = "RALPH_ROOM")]
    pub room_id: String,

    /// Username to register with
    #[arg(env = "RALPH_USERNAME")]
    pub username: String,

    /// Claude model to use
    #[arg(long, default_value = "opus", env = "RALPH_MODEL")]
    pub model: String,

    /// GitHub issue number — enables progress file persistence
    #[arg(long, env = "RALPH_ISSUE")]
    pub issue: Option<String>,

    /// Run in a detached tmux session (ralph-<username>)
    #[arg(long)]
    pub tmux: bool,

    /// Max iterations before stopping (0 = unlimited)
    #[arg(long, default_value_t = 50)]
    pub max_iter: u32,

    /// Seconds between iterations
    #[arg(long, default_value_t = 5)]
    pub cooldown: u64,

    /// Custom system prompt file (replaces built-in prompt)
    #[arg(long)]
    pub prompt: Option<PathBuf>,

    /// Personality file — contents prepended to the system prompt
    #[arg(long)]
    pub personality: Option<PathBuf>,

    /// Additional directories for claude --add-dir (repeatable)
    #[arg(long = "add-dir")]
    pub add_dirs: Vec<PathBuf>,

    /// Allowed tools for claude (comma-separated, passed as --allowedTools)
    #[arg(long = "allow-tools", value_delimiter = ',')]
    pub allow_tools: Vec<String>,

    /// Print the prompt that would be sent, then exit
    #[arg(long)]
    pub dry_run: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::Mutex;

    /// Mutex to serialize env-var tests (env is process-global state).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: clear all RALPH_* env vars to avoid cross-test contamination.
    fn clear_ralph_env() {
        for key in ["RALPH_ROOM", "RALPH_USERNAME", "RALPH_MODEL", "RALPH_ISSUE"] {
            unsafe { std::env::remove_var(key) };
        }
    }

    #[test]
    fn cli_args_take_precedence_over_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_ralph_env();

        unsafe {
            std::env::set_var("RALPH_ROOM", "env-room");
            std::env::set_var("RALPH_USERNAME", "env-user");
            std::env::set_var("RALPH_MODEL", "env-model");
            std::env::set_var("RALPH_ISSUE", "99");
        }

        let cli = Cli::try_parse_from([
            "room-ralph",
            "cli-room",
            "cli-user",
            "--model",
            "cli-model",
            "--issue",
            "42",
        ])
        .unwrap();

        assert_eq!(cli.room_id, "cli-room");
        assert_eq!(cli.username, "cli-user");
        assert_eq!(cli.model, "cli-model");
        assert_eq!(cli.issue.as_deref(), Some("42"));

        clear_ralph_env();
    }

    #[test]
    fn env_vars_used_when_args_omitted() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_ralph_env();

        unsafe {
            std::env::set_var("RALPH_ROOM", "env-room");
            std::env::set_var("RALPH_USERNAME", "env-user");
            std::env::set_var("RALPH_MODEL", "haiku");
            std::env::set_var("RALPH_ISSUE", "77");
        }

        let cli = Cli::try_parse_from(["room-ralph"]).unwrap();

        assert_eq!(cli.room_id, "env-room");
        assert_eq!(cli.username, "env-user");
        assert_eq!(cli.model, "haiku");
        assert_eq!(cli.issue.as_deref(), Some("77"));

        clear_ralph_env();
    }

    #[test]
    fn missing_required_without_env_fails() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_ralph_env();

        let result = Cli::try_parse_from(["room-ralph"]);
        assert!(result.is_err(), "should fail without room_id or RALPH_ROOM");

        clear_ralph_env();
    }

    #[test]
    fn partial_env_with_partial_args() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_ralph_env();

        unsafe {
            std::env::set_var("RALPH_USERNAME", "env-user");
        }

        // room_id from CLI, username from env
        let cli = Cli::try_parse_from(["room-ralph", "cli-room"]).unwrap();

        assert_eq!(cli.room_id, "cli-room");
        assert_eq!(cli.username, "env-user");
        assert_eq!(cli.model, "opus"); // default
        assert!(cli.issue.is_none());

        clear_ralph_env();
    }

    #[test]
    fn model_default_used_without_env_or_flag() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_ralph_env();

        let cli = Cli::try_parse_from(["room-ralph", "myroom", "myuser"]).unwrap();

        assert_eq!(cli.model, "opus");
        assert!(cli.issue.is_none());

        clear_ralph_env();
    }

    #[test]
    fn issue_env_is_optional() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_ralph_env();

        unsafe {
            std::env::set_var("RALPH_ROOM", "r");
            std::env::set_var("RALPH_USERNAME", "u");
        }

        let cli = Cli::try_parse_from(["room-ralph"]).unwrap();
        assert!(
            cli.issue.is_none(),
            "issue should be None when RALPH_ISSUE unset"
        );

        clear_ralph_env();
    }
}
