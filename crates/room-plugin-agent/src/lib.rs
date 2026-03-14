use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use room_protocol::plugin::{
    BoxFuture, CommandContext, CommandInfo, ParamSchema, ParamType, Plugin, PluginResult,
};

/// Grace period in seconds before sending SIGKILL after SIGTERM.
const STOP_GRACE_PERIOD_SECS: u64 = 5;

/// Built-in personality names for `/spawn` autocomplete.
///
/// These must match the compiled-in personalities in `room-ralph/src/personalities.rs`.
/// The list is duplicated here because `room-plugin-agent` must not depend on
/// `room-ralph` (the plugin runs inside the broker; ralph is a client).
const PERSONALITY_NAMES: &[&str] = &[
    "coder",
    "reviewer",
    "researcher",
    "coordinator",
    "documenter",
];

/// A spawned agent process tracked by the plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnedAgent {
    pub username: String,
    pub pid: u32,
    pub model: String,
    #[serde(default)]
    pub personality: String,
    pub spawned_at: DateTime<Utc>,
    pub log_path: PathBuf,
    pub room_id: String,
}

/// Agent spawn/stop/list plugin.
///
/// Manages ralph agent processes spawned from within a room. Tracks PIDs,
/// provides `/agent spawn`, `/agent list`, and `/agent stop` commands.
pub struct AgentPlugin {
    /// Running agents keyed by username.
    agents: Arc<Mutex<HashMap<String, SpawnedAgent>>>,
    /// Child process handles for exit code tracking (not serialized).
    children: Arc<Mutex<HashMap<String, Child>>>,
    /// Recorded exit codes for agents whose Child handles have been reaped.
    exit_codes: Arc<Mutex<HashMap<String, Option<i32>>>>,
    /// Path to persist agent state (e.g. `~/.room/state/agents-<room>.json`).
    state_path: PathBuf,
    /// Socket path to pass to spawned ralph processes.
    socket_path: PathBuf,
    /// Directory for agent log files.
    log_dir: PathBuf,
}

impl AgentPlugin {
    /// Create a new agent plugin.
    ///
    /// Loads previously persisted agent state and prunes entries whose
    /// processes are no longer running.
    pub fn new(state_path: PathBuf, socket_path: PathBuf, log_dir: PathBuf) -> Self {
        let agents = load_agents(&state_path);
        // Prune dead agents on startup
        let agents: HashMap<String, SpawnedAgent> = agents
            .into_iter()
            .filter(|(_, a)| is_process_alive(a.pid))
            .collect();
        let plugin = Self {
            agents: Arc::new(Mutex::new(agents)),
            children: Arc::new(Mutex::new(HashMap::new())),
            exit_codes: Arc::new(Mutex::new(HashMap::new())),
            state_path,
            socket_path,
            log_dir,
        };
        plugin.persist();
        plugin
    }

    /// Returns the command info for the TUI command palette without needing
    /// an instantiated plugin. Used by `all_known_commands()`.
    pub fn default_commands() -> Vec<CommandInfo> {
        vec![
            CommandInfo {
                name: "agent".to_owned(),
                description: "Spawn, list, or stop ralph agents".to_owned(),
                usage: "/agent <action> [args...]".to_owned(),
                params: vec![
                    ParamSchema {
                        name: "action".to_owned(),
                        param_type: ParamType::Choice(vec![
                            "spawn".to_owned(),
                            "list".to_owned(),
                            "stop".to_owned(),
                        ]),
                        required: true,
                        description: "Subcommand".to_owned(),
                    },
                    ParamSchema {
                        name: "args".to_owned(),
                        param_type: ParamType::Text,
                        required: false,
                        description: "Arguments for the subcommand".to_owned(),
                    },
                ],
            },
            CommandInfo {
                name: "spawn".to_owned(),
                description: "Spawn an agent by personality name".to_owned(),
                usage: "/spawn <personality> [--name <username>]".to_owned(),
                params: vec![
                    ParamSchema {
                        name: "personality".to_owned(),
                        param_type: ParamType::Choice(
                            PERSONALITY_NAMES.iter().map(|s| (*s).to_owned()).collect(),
                        ),
                        required: true,
                        description: "Personality preset to use".to_owned(),
                    },
                    ParamSchema {
                        name: "name".to_owned(),
                        param_type: ParamType::Text,
                        required: false,
                        description: "Override auto-generated username".to_owned(),
                    },
                ],
            },
        ]
    }

    fn persist(&self) {
        let agents = self.agents.lock().unwrap();
        if let Ok(json) = serde_json::to_string_pretty(&*agents) {
            let _ = fs::write(&self.state_path, json);
        }
    }

    fn handle_spawn(&self, ctx: &CommandContext) -> Result<String, String> {
        let params = &ctx.params;
        if params.len() < 2 {
            return Err(
                "usage: /agent spawn <username> [--model <model>] [--personality <name>] [--issue <N>] [--prompt <text>]"
                    .to_owned(),
            );
        }

        let username = &params[1];

        // Validate username is not empty
        if username.is_empty() || username.starts_with('-') {
            return Err("invalid username".to_owned());
        }

        // Check for collision with online users
        if ctx
            .metadata
            .online_users
            .iter()
            .any(|u| u.username == *username)
        {
            return Err(format!("username '{username}' is already online"));
        }

        // Check for collision with already-spawned agents
        {
            let agents = self.agents.lock().unwrap();
            if agents.contains_key(username.as_str()) {
                return Err(format!(
                    "agent '{username}' is already running (pid {})",
                    agents[username.as_str()].pid
                ));
            }
        }

        // Parse optional flags from params[2..]
        let mut model = "sonnet".to_owned();
        let mut personality = String::new();
        let mut issue: Option<String> = None;
        let mut prompt: Option<String> = None;

        let mut i = 2;
        while i < params.len() {
            match params[i].as_str() {
                "--model" => {
                    i += 1;
                    if i < params.len() {
                        model = params[i].clone();
                    }
                }
                "--personality" => {
                    i += 1;
                    if i < params.len() {
                        personality = params[i].clone();
                    }
                }
                "--issue" => {
                    i += 1;
                    if i < params.len() {
                        issue = Some(params[i].clone());
                    }
                }
                "--prompt" => {
                    i += 1;
                    if i < params.len() {
                        prompt = Some(params[i].clone());
                    }
                }
                _ => {}
            }
            i += 1;
        }

        // Create log directory
        let _ = fs::create_dir_all(&self.log_dir);

        let ts = Utc::now().format("%Y%m%d-%H%M%S");
        let log_path = self.log_dir.join(format!("agent-{username}-{ts}.log"));

        let log_file =
            fs::File::create(&log_path).map_err(|e| format!("failed to create log file: {e}"))?;
        let stderr_file = log_file
            .try_clone()
            .map_err(|e| format!("failed to clone log file handle: {e}"))?;

        // Build the room-ralph command
        let mut cmd = Command::new("room-ralph");
        cmd.arg(&ctx.room_id)
            .arg(username)
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--model")
            .arg(&model);

        if let Some(ref iss) = issue {
            cmd.arg("--issue").arg(iss);
        }
        if let Some(ref p) = prompt {
            cmd.arg("--prompt").arg(p);
        }
        if !personality.is_empty() {
            cmd.arg("--personality").arg(&personality);
        }

        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(stderr_file));

        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn room-ralph: {e}"))?;

        let pid = child.id();

        let agent = SpawnedAgent {
            username: username.clone(),
            pid,
            model: model.clone(),
            personality: personality.clone(),
            spawned_at: Utc::now(),
            log_path: log_path.clone(),
            room_id: ctx.room_id.clone(),
        };

        {
            let mut agents = self.agents.lock().unwrap();
            agents.insert(username.clone(), agent);
        }
        {
            let mut children = self.children.lock().unwrap();
            children.insert(username.clone(), child);
        }
        self.persist();

        let personality_info = if personality.is_empty() {
            String::new()
        } else {
            format!(", personality: {personality}")
        };
        Ok(format!(
            "agent {username} spawned (pid {pid}, model: {model}{personality_info})"
        ))
    }

    fn handle_list(&self) -> String {
        let agents = self.agents.lock().unwrap();
        if agents.is_empty() {
            return "no agents spawned".to_owned();
        }

        let mut lines =
            vec!["username     | pid   | personality | model  | uptime  | status".to_owned()];

        // Try to reap exit codes from child handles.
        {
            let mut children = self.children.lock().unwrap();
            let mut exit_codes = self.exit_codes.lock().unwrap();
            let usernames: Vec<String> = children.keys().cloned().collect();
            for name in usernames {
                if let Some(child) = children.get_mut(&name) {
                    if let Ok(Some(status)) = child.try_wait() {
                        exit_codes.insert(name.clone(), status.code());
                        children.remove(&name);
                    }
                }
            }
        }

        let exit_codes = self.exit_codes.lock().unwrap();
        let now = Utc::now();
        let mut entries: Vec<_> = agents.values().collect();
        entries.sort_by_key(|a| a.spawned_at);

        for agent in entries {
            let uptime = format_duration(now - agent.spawned_at);
            let status = if is_process_alive(agent.pid) {
                "running".to_owned()
            } else if let Some(code) = exit_codes.get(&agent.username) {
                match code {
                    Some(c) => format!("exited ({c})"),
                    None => "exited (signal)".to_owned(),
                }
            } else {
                "exited (unknown)".to_owned()
            };
            let personality_display = if agent.personality.is_empty() {
                "-"
            } else {
                &agent.personality
            };
            lines.push(format!(
                "{:<12} | {:<5} | {:<11} | {:<6} | {:<7} | {}",
                agent.username, agent.pid, personality_display, agent.model, uptime, status,
            ));
        }

        lines.join("\n")
    }

    /// Handle `/spawn <personality> [--name <username>]`.
    ///
    /// Expands to the equivalent `/agent spawn` invocation using the
    /// personality's defaults. The personality name is passed to `room-ralph`
    /// via `--personality`, which handles prompt/profile/model resolution.
    fn handle_spawn_personality(&self, ctx: &CommandContext) -> Result<String, String> {
        let personality = ctx.params.first().ok_or_else(|| {
            format!(
                "usage: /spawn <personality> [--name <username>]\navailable: {}",
                PERSONALITY_NAMES.join(", ")
            )
        })?;

        if !PERSONALITY_NAMES.contains(&personality.as_str()) {
            return Err(format!(
                "unknown personality '{}'. available: {}",
                personality,
                PERSONALITY_NAMES.join(", ")
            ));
        }

        // Parse optional --name flag
        let mut username: Option<String> = None;
        let mut i = 1;
        while i < ctx.params.len() {
            if ctx.params[i] == "--name" {
                i += 1;
                if i < ctx.params.len() {
                    username = Some(ctx.params[i].clone());
                }
            }
            i += 1;
        }

        // Auto-generate username if not provided: <personality>-<short-id>
        let username = username.unwrap_or_else(|| {
            let short_id = &ctx.message_id[..4.min(ctx.message_id.len())];
            format!("{personality}-{short_id}")
        });

        if username.is_empty() || username.starts_with('-') {
            return Err("invalid username".to_owned());
        }

        // Check for collision with online users
        if ctx
            .metadata
            .online_users
            .iter()
            .any(|u| u.username == username)
        {
            return Err(format!("username '{username}' is already online"));
        }

        // Check for collision with already-spawned agents
        {
            let agents = self.agents.lock().unwrap();
            if agents.contains_key(username.as_str()) {
                return Err(format!(
                    "agent '{username}' is already running (pid {})",
                    agents[username.as_str()].pid
                ));
            }
        }

        // Create log directory
        let _ = fs::create_dir_all(&self.log_dir);

        let ts = Utc::now().format("%Y%m%d-%H%M%S");
        let log_path = self.log_dir.join(format!("agent-{username}-{ts}.log"));

        let log_file =
            fs::File::create(&log_path).map_err(|e| format!("failed to create log file: {e}"))?;
        let stderr_file = log_file
            .try_clone()
            .map_err(|e| format!("failed to clone log file handle: {e}"))?;

        // Build the room-ralph command with --personality
        let mut cmd = Command::new("room-ralph");
        cmd.arg(&ctx.room_id)
            .arg(&username)
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--personality")
            .arg(personality);

        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(stderr_file));

        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn room-ralph: {e}"))?;

        let pid = child.id();

        let agent = SpawnedAgent {
            username: username.clone(),
            pid,
            model: format!("{personality}/*"),
            personality: personality.to_owned(),
            spawned_at: Utc::now(),
            log_path,
            room_id: ctx.room_id.clone(),
        };

        {
            let mut agents = self.agents.lock().unwrap();
            agents.insert(username.clone(), agent);
        }
        {
            let mut children = self.children.lock().unwrap();
            children.insert(username.clone(), child);
        }
        self.persist();

        Ok(format!(
            "agent {username} spawned (pid {pid}, personality: {personality})"
        ))
    }

    fn handle_stop(&self, ctx: &CommandContext) -> Result<String, String> {
        if ctx.params.len() < 2 {
            return Err("usage: /agent stop <username>".to_owned());
        }

        // Host-only permission check
        if let Some(ref host) = ctx.metadata.host {
            if ctx.sender != *host {
                return Err("permission denied: only the host can stop agents".to_owned());
            }
        }

        let username = &ctx.params[1];

        let agent = {
            let agents = self.agents.lock().unwrap();
            agents.get(username.as_str()).cloned()
        };

        let Some(agent) = agent else {
            return Err(format!("no agent named '{username}'"));
        };

        // Check if already exited before attempting to stop
        let was_alive = is_process_alive(agent.pid);
        if was_alive {
            // Try to use stored Child handle for clean shutdown, fall back to PID signal.
            let mut child = {
                let mut children = self.children.lock().unwrap();
                children.remove(username.as_str())
            };
            if let Some(ref mut child) = child {
                let _ = child.kill();
                let _ = child.wait();
            } else {
                stop_process(agent.pid, STOP_GRACE_PERIOD_SECS);
            }
        }

        {
            let mut agents = self.agents.lock().unwrap();
            agents.remove(username.as_str());
        }
        {
            let mut exit_codes = self.exit_codes.lock().unwrap();
            exit_codes.remove(username.as_str());
        }
        self.persist();

        if was_alive {
            Ok(format!(
                "agent {} stopped by {} (was pid {})",
                username, ctx.sender, agent.pid
            ))
        } else {
            Ok(format!(
                "agent {} removed (already exited, was pid {})",
                username, agent.pid
            ))
        }
    }
}

impl Plugin for AgentPlugin {
    fn name(&self) -> &str {
        "agent"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn commands(&self) -> Vec<CommandInfo> {
        Self::default_commands()
    }

    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
        Box::pin(async move {
            // `/spawn <personality>` is dispatched here with command == "spawn".
            if ctx.command == "spawn" {
                return match self.handle_spawn_personality(&ctx) {
                    Ok(msg) => Ok(PluginResult::Broadcast(msg)),
                    Err(e) => Ok(PluginResult::Reply(e)),
                };
            }

            // `/agent <action> [args...]`
            let action = ctx.params.first().map(|s| s.as_str()).unwrap_or("");

            match action {
                "spawn" => match self.handle_spawn(&ctx) {
                    Ok(msg) => Ok(PluginResult::Broadcast(msg)),
                    Err(e) => Ok(PluginResult::Reply(e)),
                },
                "list" => Ok(PluginResult::Reply(self.handle_list())),
                "stop" => match self.handle_stop(&ctx) {
                    Ok(msg) => Ok(PluginResult::Broadcast(msg)),
                    Err(e) => Ok(PluginResult::Reply(e)),
                },
                _ => Ok(PluginResult::Reply(
                    "unknown action. usage: /agent spawn|list|stop".to_owned(),
                )),
            }
        })
    }
}

/// Check whether a process with the given PID is still running.
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) checks existence without sending a signal
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Send SIGTERM to a process, wait `grace_secs`, then SIGKILL if still alive.
fn stop_process(pid: u32, grace_secs: u64) {
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_secs(grace_secs));
        if is_process_alive(pid) {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, grace_secs);
    }
}

/// Format a chrono Duration as a human-readable string (e.g. "14m", "2h").
fn format_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Load agent state from a JSON file, returning an empty map on error.
fn load_agents(path: &std::path::Path) -> HashMap<String, SpawnedAgent> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use room_protocol::plugin::{RoomMetadata, UserInfo};

    fn test_plugin(dir: &std::path::Path) -> AgentPlugin {
        AgentPlugin::new(
            dir.join("agents.json"),
            dir.join("room.sock"),
            dir.join("logs"),
        )
    }

    fn make_ctx(_plugin: &AgentPlugin, params: Vec<&str>, online: Vec<&str>) -> CommandContext {
        CommandContext {
            command: "agent".to_owned(),
            params: params.into_iter().map(|s| s.to_owned()).collect(),
            sender: "host".to_owned(),
            room_id: "test-room".to_owned(),
            message_id: "msg-1".to_owned(),
            timestamp: Utc::now(),
            history: Box::new(NoopHistory),
            writer: Box::new(NoopWriter),
            metadata: RoomMetadata {
                online_users: online
                    .into_iter()
                    .map(|u| UserInfo {
                        username: u.to_owned(),
                        status: String::new(),
                    })
                    .collect(),
                host: Some("host".to_owned()),
                message_count: 0,
            },
            available_commands: vec![],
            team_access: None,
        }
    }

    // Noop implementations for test contexts
    struct NoopHistory;
    impl room_protocol::plugin::HistoryAccess for NoopHistory {
        fn all(&self) -> BoxFuture<'_, anyhow::Result<Vec<room_protocol::Message>>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn tail(&self, _n: usize) -> BoxFuture<'_, anyhow::Result<Vec<room_protocol::Message>>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn since(
            &self,
            _message_id: &str,
        ) -> BoxFuture<'_, anyhow::Result<Vec<room_protocol::Message>>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn count(&self) -> BoxFuture<'_, anyhow::Result<usize>> {
            Box::pin(async { Ok(0) })
        }
    }

    struct NoopWriter;
    impl room_protocol::plugin::MessageWriter for NoopWriter {
        fn broadcast(&self, _content: &str) -> BoxFuture<'_, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn reply_to(&self, _user: &str, _content: &str) -> BoxFuture<'_, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn emit_event(
            &self,
            _event_type: room_protocol::EventType,
            _content: &str,
            _params: Option<serde_json::Value>,
        ) -> BoxFuture<'_, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn spawn_missing_username() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let ctx = make_ctx(&plugin, vec!["spawn"], vec![]);
        let result = plugin.handle_spawn(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("usage"));
    }

    #[test]
    fn spawn_invalid_username() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let ctx = make_ctx(&plugin, vec!["spawn", "--badname"], vec![]);
        let result = plugin.handle_spawn(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid username"));
    }

    #[test]
    fn spawn_username_collision_with_online_user() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let ctx = make_ctx(&plugin, vec!["spawn", "alice"], vec!["alice", "bob"]);
        let result = plugin.handle_spawn(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already online"));
    }

    #[test]
    fn spawn_username_collision_with_running_agent() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        // Manually insert a fake running agent (use our own PID so it appears alive)
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: std::process::id(),
                    model: "sonnet".to_owned(),
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let ctx = make_ctx(&plugin, vec!["spawn", "bot1"], vec![]);
        let result = plugin.handle_spawn(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already running"));
    }

    #[test]
    fn list_empty() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        assert_eq!(plugin.handle_list(), "no agents spawned");
    }

    #[test]
    fn list_with_agents() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: 99999,
                    model: "opus".to_owned(),
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let output = plugin.handle_list();
        assert!(output.contains("bot1"));
        assert!(output.contains("opus"));
        assert!(output.contains("99999"));
    }

    #[test]
    fn stop_missing_username() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let ctx = make_ctx(&plugin, vec!["stop"], vec![]);
        let result = plugin.handle_stop(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("usage"));
    }

    #[test]
    fn stop_unknown_agent() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let ctx = make_ctx(&plugin, vec!["stop", "nobody"], vec![]);
        let result = plugin.handle_stop(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no agent named"));
    }

    #[test]
    fn stop_non_host_denied() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        // Insert a fake agent
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: std::process::id(),
                    model: "sonnet".to_owned(),
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        // Create context with sender != host
        let mut ctx = make_ctx(&plugin, vec!["stop", "bot1"], vec![]);
        ctx.sender = "not-host".to_owned();
        let result = plugin.handle_stop(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("permission denied"));
    }

    #[test]
    fn stop_already_exited_agent() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        // Insert an agent with a dead PID
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "dead-bot".to_owned(),
                SpawnedAgent {
                    username: "dead-bot".to_owned(),
                    pid: 999_999_999,
                    model: "haiku".to_owned(),
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let ctx = make_ctx(&plugin, vec!["stop", "dead-bot"], vec![]);
        let result = plugin.handle_stop(&ctx);
        assert!(result.is_ok());
        let msg = result.unwrap();
        assert!(msg.contains("already exited"));
        assert!(msg.contains("removed"));

        // Agent should be removed from tracking
        let agents = plugin.agents.lock().unwrap();
        assert!(!agents.contains_key("dead-bot"));
    }

    #[test]
    fn stop_host_can_stop_agent() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        // Insert an agent with a dead PID (safe to "stop")
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: 999_999_999,
                    model: "sonnet".to_owned(),
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        // Host (default sender) should be able to stop
        let ctx = make_ctx(&plugin, vec!["stop", "bot1"], vec![]);
        let result = plugin.handle_stop(&ctx);
        assert!(result.is_ok());

        let agents = plugin.agents.lock().unwrap();
        assert!(!agents.contains_key("bot1"));
    }

    #[test]
    fn persist_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("agents.json");

        // Create plugin and add an agent
        let plugin = AgentPlugin::new(
            state_path.clone(),
            dir.path().join("room.sock"),
            dir.path().join("logs"),
        );
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: std::process::id(), // use own PID so it appears alive
                    model: "sonnet".to_owned(),
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }
        plugin.persist();

        // Load a new plugin from same state — should find the agent
        let plugin2 = AgentPlugin::new(
            state_path,
            dir.path().join("room.sock"),
            dir.path().join("logs"),
        );
        let agents = plugin2.agents.lock().unwrap();
        assert!(agents.contains_key("bot1"));
    }

    #[test]
    fn prune_dead_agents_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("agents.json");

        // Write a state file with a dead PID
        let mut agents = HashMap::new();
        agents.insert(
            "dead-bot".to_owned(),
            SpawnedAgent {
                username: "dead-bot".to_owned(),
                pid: 999_999_999, // very unlikely to be alive
                model: "haiku".to_owned(),
                personality: String::new(),
                spawned_at: Utc::now(),
                log_path: PathBuf::from("/tmp/test.log"),
                room_id: "test-room".to_owned(),
            },
        );
        fs::write(&state_path, serde_json::to_string(&agents).unwrap()).unwrap();

        // New plugin should prune the dead agent
        let plugin = AgentPlugin::new(
            state_path,
            dir.path().join("room.sock"),
            dir.path().join("logs"),
        );
        let agents = plugin.agents.lock().unwrap();
        assert!(agents.is_empty(), "dead agents should be pruned on load");
    }

    // ── /spawn command schema tests ─────────────────────────────────────

    #[test]
    fn default_commands_includes_spawn() {
        let cmds = AgentPlugin::default_commands();
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"spawn"),
            "default_commands must include spawn"
        );
    }

    #[test]
    fn spawn_command_has_personality_choice_param() {
        let cmds = AgentPlugin::default_commands();
        let spawn = cmds.iter().find(|c| c.name == "spawn").unwrap();
        assert_eq!(spawn.params.len(), 2);
        match &spawn.params[0].param_type {
            ParamType::Choice(values) => {
                assert!(values.contains(&"coder".to_owned()));
                assert!(values.contains(&"reviewer".to_owned()));
                assert!(values.contains(&"researcher".to_owned()));
                assert!(values.contains(&"coordinator".to_owned()));
                assert!(values.contains(&"documenter".to_owned()));
                assert_eq!(values.len(), 5);
            }
            other => panic!("expected Choice, got {:?}", other),
        }
    }

    #[test]
    fn spawn_personality_unknown_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let mut ctx = make_ctx(&plugin, vec!["hacker"], vec![]);
        ctx.command = "spawn".to_owned();
        let result = plugin.handle_spawn_personality(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown personality"));
    }

    #[test]
    fn spawn_personality_missing_returns_usage() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let mut ctx = make_ctx(&plugin, vec![] as Vec<&str>, vec![]);
        ctx.command = "spawn".to_owned();
        let result = plugin.handle_spawn_personality(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("usage"));
    }

    #[test]
    fn spawn_personality_collision_with_online_user() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let mut ctx = make_ctx(&plugin, vec!["coder", "--name", "alice"], vec!["alice"]);
        ctx.command = "spawn".to_owned();
        let result = plugin.handle_spawn_personality(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already online"));
    }

    #[test]
    fn spawn_personality_collision_with_auto_generated_name() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        // Pre-insert an agent with the auto-generated name
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "coder-abcd".to_owned(),
                SpawnedAgent {
                    username: "coder-abcd".to_owned(),
                    pid: std::process::id(),
                    model: "sonnet".to_owned(),
                    personality: "coder".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let mut ctx = make_ctx(&plugin, vec!["coder"], vec![]);
        ctx.command = "spawn".to_owned();
        ctx.message_id = "abcd-1234".to_owned();
        // Auto-generated name would be "coder-abcd" which collides
        let result = plugin.handle_spawn_personality(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already running"));
    }

    #[test]
    fn unknown_action_returns_usage() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let ctx = make_ctx(&plugin, vec!["frobnicate"], vec![]);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(plugin.handle(ctx)).unwrap();
        match result {
            PluginResult::Reply(msg) => assert!(msg.contains("unknown action")),
            PluginResult::Broadcast(_) => panic!("expected Reply, got Broadcast"),
            PluginResult::Handled => panic!("expected Reply, got Handled"),
        }
    }

    // ── /agent list tests (#689) ──────────────────────────────────────────

    #[test]
    fn list_header_includes_personality_column() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: std::process::id(),
                    model: "sonnet".to_owned(),
                    personality: "coder".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let output = plugin.handle_list();
        let header = output.lines().next().unwrap();
        assert!(
            header.contains("personality"),
            "header must include personality column"
        );
        assert!(output.contains("coder"), "personality value must appear");
    }

    #[test]
    fn list_shows_dash_for_empty_personality() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: std::process::id(),
                    model: "opus".to_owned(),
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let output = plugin.handle_list();
        // The personality column should show "-" for empty personality.
        let data_line = output.lines().nth(1).unwrap();
        assert!(
            data_line.contains("| -"),
            "empty personality should show '-'"
        );
    }

    #[test]
    fn list_shows_running_for_alive_process() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: std::process::id(), // our own PID — always alive
                    model: "sonnet".to_owned(),
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let output = plugin.handle_list();
        assert!(
            output.contains("running"),
            "alive process should show 'running'"
        );
    }

    #[test]
    fn list_shows_exited_unknown_for_dead_process_without_child() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: 999_999_999, // not alive
                    model: "haiku".to_owned(),
                    personality: "scout".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let output = plugin.handle_list();
        assert!(
            output.contains("exited (unknown)"),
            "dead process without child handle should show 'exited (unknown)'"
        );
    }

    #[test]
    fn list_shows_exit_code_when_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: 999_999_999,
                    model: "sonnet".to_owned(),
                    personality: "coder".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }
        {
            let mut exit_codes = plugin.exit_codes.lock().unwrap();
            exit_codes.insert("bot1".to_owned(), Some(0));
        }

        let output = plugin.handle_list();
        assert!(
            output.contains("exited (0)"),
            "recorded exit code should appear in output"
        );
    }

    #[test]
    fn list_shows_signal_when_no_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: 999_999_999,
                    model: "sonnet".to_owned(),
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }
        {
            // None exit code = killed by signal
            let mut exit_codes = plugin.exit_codes.lock().unwrap();
            exit_codes.insert("bot1".to_owned(), None);
        }

        let output = plugin.handle_list();
        assert!(
            output.contains("exited (signal)"),
            "signal death should show 'exited (signal)'"
        );
    }

    #[test]
    fn list_sorts_by_spawn_time() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let now = Utc::now();

        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "second".to_owned(),
                SpawnedAgent {
                    username: "second".to_owned(),
                    pid: std::process::id(),
                    model: "opus".to_owned(),
                    personality: String::new(),
                    spawned_at: now,
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
            agents.insert(
                "first".to_owned(),
                SpawnedAgent {
                    username: "first".to_owned(),
                    pid: std::process::id(),
                    model: "sonnet".to_owned(),
                    personality: String::new(),
                    spawned_at: now - chrono::Duration::minutes(5),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let output = plugin.handle_list();
        let lines: Vec<&str> = output.lines().collect();
        // Skip header (line 0), first data line should be "first", second "second".
        assert!(
            lines[1].contains("first"),
            "older agent should appear first"
        );
        assert!(
            lines[2].contains("second"),
            "newer agent should appear second"
        );
    }

    #[test]
    fn list_with_personality_and_exit_code_full_row() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "reviewer-a1".to_owned(),
                SpawnedAgent {
                    username: "reviewer-a1".to_owned(),
                    pid: 999_999_999,
                    model: "sonnet".to_owned(),
                    personality: "reviewer".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }
        {
            let mut exit_codes = plugin.exit_codes.lock().unwrap();
            exit_codes.insert("reviewer-a1".to_owned(), Some(0));
        }

        let output = plugin.handle_list();
        assert!(output.contains("reviewer-a1"));
        assert!(output.contains("reviewer"));
        assert!(output.contains("sonnet"));
        assert!(output.contains("exited (0)"));
    }

    #[test]
    fn persist_roundtrip_with_personality() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("agents.json");

        let plugin = AgentPlugin::new(
            state_path.clone(),
            dir.path().join("room.sock"),
            dir.path().join("logs"),
        );
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "bot1".to_owned(),
                SpawnedAgent {
                    username: "bot1".to_owned(),
                    pid: std::process::id(),
                    model: "sonnet".to_owned(),
                    personality: "coder".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }
        plugin.persist();

        // Reload — personality should survive roundtrip.
        let plugin2 = AgentPlugin::new(
            state_path,
            dir.path().join("room.sock"),
            dir.path().join("logs"),
        );
        let agents = plugin2.agents.lock().unwrap();
        assert_eq!(agents["bot1"].personality, "coder");
    }
}
