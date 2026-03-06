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
    pub room_id: String,

    /// Username to register with
    pub username: String,

    /// Claude model to use
    #[arg(long, default_value = "opus")]
    pub model: String,

    /// GitHub issue number — enables progress file persistence
    #[arg(long)]
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
