pub mod personalities;

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
use room_protocol::Message;

// ── C ABI entry points for cdylib loading ─────────────────────────────────

/// JSON configuration for the agent plugin when loaded dynamically.
///
/// ```json
/// {
///   "state_path": "/home/user/.room/state/agents-myroom.json",
///   "socket_path": "/tmp/room-myroom.sock",
///   "log_dir": "/home/user/.room/logs"
/// }
/// ```
#[derive(Deserialize)]
struct AgentConfig {
    state_path: PathBuf,
    socket_path: PathBuf,
    log_dir: PathBuf,
}

/// Create an [`AgentPlugin`] from a JSON config string.
///
/// Falls back to temp-path defaults if config is empty (for testing).
fn create_agent_from_config(config: &str) -> AgentPlugin {
    if config.is_empty() {
        AgentPlugin::new(
            PathBuf::from("/tmp/room-agents.json"),
            PathBuf::from("/tmp/room-default.sock"),
            PathBuf::from("/tmp/room-logs"),
        )
    } else {
        let cfg: AgentConfig =
            serde_json::from_str(config).expect("invalid agent plugin config JSON");
        AgentPlugin::new(cfg.state_path, cfg.socket_path, cfg.log_dir)
    }
}

room_protocol::declare_plugin!("agent", create_agent_from_config);

/// Grace period in seconds before sending SIGKILL after SIGTERM.
const STOP_GRACE_PERIOD_SECS: u64 = 5;

/// Default number of log lines to show.
const DEFAULT_TAIL_LINES: usize = 20;

/// Default threshold in seconds before an agent is considered stale.
const DEFAULT_STALE_THRESHOLD_SECS: i64 = 300;

/// Health status of a spawned agent.
#[derive(Debug, Clone, PartialEq)]
pub enum HealthStatus {
    /// Agent is active — sent a message recently.
    Healthy,
    /// Agent has not sent any message within the stale threshold.
    Stale,
    /// Agent process has exited.
    Exited(Option<i32>),
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthStatus::Healthy => write!(f, "healthy"),
            HealthStatus::Stale => write!(f, "stale"),
            HealthStatus::Exited(Some(code)) => write!(f, "exited ({code})"),
            HealthStatus::Exited(None) => write!(f, "exited (signal)"),
        }
    }
}

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
    /// Last time each tracked agent sent a message, keyed by username.
    last_seen_at: Arc<Mutex<HashMap<String, DateTime<Utc>>>>,
    /// Seconds of inactivity before an agent is marked stale.
    stale_threshold_secs: i64,
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
            last_seen_at: Arc::new(Mutex::new(HashMap::new())),
            stale_threshold_secs: DEFAULT_STALE_THRESHOLD_SECS,
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
                description: "Spawn, list, stop, or tail logs of ralph agents".to_owned(),
                usage: "/agent <action> [args...]".to_owned(),
                params: vec![
                    ParamSchema {
                        name: "action".to_owned(),
                        param_type: ParamType::Choice(vec![
                            "spawn".to_owned(),
                            "list".to_owned(),
                            "stop".to_owned(),
                            "logs".to_owned(),
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
                        param_type: ParamType::Choice(personalities::all_personality_names()),
                        required: true,
                        description: "Personality preset name".to_owned(),
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

    fn handle_spawn(&self, ctx: &CommandContext) -> Result<(String, serde_json::Value), String> {
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
        set_process_group(&mut cmd);

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
        let text =
            format!("agent {username} spawned (pid {pid}, model: {model}{personality_info})");
        let data = serde_json::json!({
            "action": "spawn",
            "username": username,
            "pid": pid,
            "model": model,
            "personality": personality,
            "log_path": log_path.to_string_lossy(),
        });
        Ok((text, data))
    }

    /// Compute the health status for a given agent.
    fn compute_health(
        &self,
        agent: &SpawnedAgent,
        exit_codes: &HashMap<String, Option<i32>>,
        now: DateTime<Utc>,
    ) -> HealthStatus {
        if !is_process_alive(agent.pid) {
            let code = exit_codes.get(&agent.username).copied().unwrap_or(None);
            return HealthStatus::Exited(code);
        }
        let last_seen = self.last_seen_at.lock().unwrap();
        if let Some(&ts) = last_seen.get(&agent.username) {
            let elapsed = (now - ts).num_seconds();
            if elapsed > self.stale_threshold_secs {
                return HealthStatus::Stale;
            }
        }
        // No message tracked yet but process is alive — healthy (just spawned).
        HealthStatus::Healthy
    }

    fn handle_list(&self) -> (String, serde_json::Value) {
        let agents = self.agents.lock().unwrap();
        if agents.is_empty() {
            let data = serde_json::json!({ "action": "list", "agents": [] });
            return ("no agents spawned".to_owned(), data);
        }

        let mut lines = vec![
            "username     | pid   | personality | model  | uptime  | health  | status".to_owned(),
        ];

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
        let mut agent_data: Vec<serde_json::Value> = Vec::new();

        for agent in entries {
            let uptime = format_duration(now - agent.spawned_at);
            let health = self.compute_health(agent, &exit_codes, now);
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
            let health_str = health.to_string();
            lines.push(format!(
                "{:<12} | {:<5} | {:<11} | {:<6} | {:<7} | {:<7} | {}",
                agent.username,
                agent.pid,
                personality_display,
                agent.model,
                uptime,
                health_str,
                status,
            ));
            agent_data.push(serde_json::json!({
                "username": agent.username,
                "pid": agent.pid,
                "model": agent.model,
                "personality": agent.personality,
                "uptime_secs": (now - agent.spawned_at).num_seconds(),
                "health": health_str,
                "status": status,
            }));
        }

        let data = serde_json::json!({ "action": "list", "agents": agent_data });
        (lines.join("\n"), data)
    }

    /// Handle `/spawn <personality> [--name <username>]`.
    ///
    /// Resolves the personality from the registry (user TOML overrides then
    /// built-in defaults), generates a username from the name pool, and
    /// spawns room-ralph with the personality's model, tool restrictions,
    /// and prompt.
    fn handle_spawn_personality(&self, ctx: &CommandContext) -> Result<String, String> {
        if ctx.params.is_empty() {
            return Err("usage: /spawn <personality> [--name <username>]".to_owned());
        }

        let personality_name = &ctx.params[0];

        let personality = personalities::resolve_personality(personality_name)
            .map_err(|e| format!("failed to load personality '{personality_name}': {e}"))?
            .ok_or_else(|| {
                let available = personalities::all_personality_names().join(", ");
                format!("unknown personality '{personality_name}'. available: {available}")
            })?;

        // Parse --name flag
        let mut explicit_name: Option<String> = None;
        let mut i = 1;
        while i < ctx.params.len() {
            if ctx.params[i] == "--name" {
                i += 1;
                if i < ctx.params.len() {
                    explicit_name = Some(ctx.params[i].clone());
                }
            }
            i += 1;
        }

        // Determine username
        let used_names: Vec<String> = {
            let agents = self.agents.lock().unwrap();
            let mut names: Vec<String> = agents.keys().cloned().collect();
            names.extend(ctx.metadata.online_users.iter().map(|u| u.username.clone()));
            names
        };

        let username = if let Some(name) = explicit_name {
            name
        } else {
            personality.generate_username(&used_names)
        };

        // Validate username
        if username.is_empty() || username.starts_with('-') {
            return Err("invalid username".to_owned());
        }

        // Check collisions
        if ctx
            .metadata
            .online_users
            .iter()
            .any(|u| u.username == username)
        {
            return Err(format!("username '{username}' is already online"));
        }
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

        // Build the room-ralph command from personality config
        let model = &personality.personality.model;
        let mut cmd = Command::new("room-ralph");
        cmd.arg(&ctx.room_id)
            .arg(&username)
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--model")
            .arg(model);

        // Apply tool restrictions
        if personality.tools.allow_all {
            cmd.arg("--allow-all");
        } else {
            if !personality.tools.disallow.is_empty() {
                cmd.arg("--disallow-tools")
                    .arg(personality.tools.disallow.join(","));
            }
            if !personality.tools.allow.is_empty() {
                cmd.arg("--allow-tools")
                    .arg(personality.tools.allow.join(","));
            }
        }

        // Apply personality template as a file — room-ralph's --personality flag
        // reads the file and prepends its contents to the default system context
        // (which includes room send/poll commands and the token). Using --prompt
        // would REPLACE the default context entirely, breaking room communication.
        if !personality.prompt.template.is_empty() {
            let template_path = self.log_dir.join(format!("{username}-personality.txt"));
            if let Err(e) = std::fs::write(&template_path, &personality.prompt.template) {
                return Err(format!("failed to write personality template: {e}"));
            }
            cmd.arg("--personality").arg(&template_path);
        }

        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(stderr_file));
        set_process_group(&mut cmd);

        let child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn room-ralph: {e}"))?;

        let pid = child.id();

        let agent = SpawnedAgent {
            username: username.clone(),
            pid,
            model: model.clone(),
            personality: personality_name.to_owned(),
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
            "agent {username} spawned via /spawn {personality_name} (pid {pid}, model: {model})"
        ))
    }

    fn handle_stop(&self, ctx: &CommandContext) -> Result<(String, serde_json::Value), String> {
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
            // Always use process group kill to terminate ralph AND all child
            // claude processes. child.kill() only kills the direct child,
            // leaving orphaned claude processes responding to messages (#777).
            stop_process(agent.pid, STOP_GRACE_PERIOD_SECS);
            // Clean up the Child handle if we have one.
            let mut child = {
                let mut children = self.children.lock().unwrap();
                children.remove(username.as_str())
            };
            if let Some(ref mut child) = child {
                let _ = child.wait();
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

        let data = serde_json::json!({
            "action": "stop",
            "username": username,
            "pid": agent.pid,
            "was_alive": was_alive,
            "stopped_by": ctx.sender,
        });
        if was_alive {
            Ok((
                format!(
                    "agent {} stopped by {} (was pid {})",
                    username, ctx.sender, agent.pid
                ),
                data,
            ))
        } else {
            Ok((
                format!(
                    "agent {} removed (already exited, was pid {})",
                    username, agent.pid
                ),
                data,
            ))
        }
    }

    fn handle_logs(&self, ctx: &CommandContext) -> Result<String, String> {
        if ctx.params.len() < 2 {
            return Err("usage: /agent logs <username> [--tail <N>]".to_owned());
        }

        let username = &ctx.params[1];

        // Parse optional --tail flag
        let mut tail_lines = DEFAULT_TAIL_LINES;
        let mut i = 2;
        while i < ctx.params.len() {
            if ctx.params[i] == "--tail" {
                i += 1;
                if i < ctx.params.len() {
                    tail_lines = ctx.params[i]
                        .parse::<usize>()
                        .map_err(|_| format!("invalid --tail value: {}", ctx.params[i]))?;
                    if tail_lines == 0 {
                        return Err("--tail must be at least 1".to_owned());
                    }
                }
            }
            i += 1;
        }

        // Look up the agent
        let agent = {
            let agents = self.agents.lock().unwrap();
            agents.get(username.as_str()).cloned()
        };

        let Some(agent) = agent else {
            return Err(format!("no agent named '{username}'"));
        };

        // Read the log file
        let content = fs::read_to_string(&agent.log_path)
            .map_err(|e| format!("cannot read log file {}: {e}", agent.log_path.display()))?;

        if content.is_empty() {
            return Ok(format!("agent {username}: log file is empty"));
        }

        // Take last N lines
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(tail_lines);
        let tail: Vec<&str> = lines[start..].to_vec();

        let header = format!(
            "agent {username} logs (last {} of {} lines):",
            tail.len(),
            lines.len()
        );
        Ok(format!("{header}\n{}", tail.join("\n")))
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

    fn on_message(&self, msg: &Message) {
        let user = msg.user();
        let agents = self.agents.lock().unwrap();
        if agents.contains_key(user) {
            drop(agents);
            let now = Utc::now();
            let mut last_seen = self.last_seen_at.lock().unwrap();
            last_seen.insert(user.to_owned(), now);
        }
    }

    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>> {
        Box::pin(async move {
            // `/spawn <personality>` is dispatched here with command == "spawn".
            if ctx.command == "spawn" {
                return match self.handle_spawn_personality(&ctx) {
                    Ok(msg) => Ok(PluginResult::Broadcast(msg, None)),
                    Err(e) => Ok(PluginResult::Reply(e, None)),
                };
            }

            // `/agent <action> [args...]`
            let action = ctx.params.first().map(|s| s.as_str()).unwrap_or("");

            match action {
                "spawn" => match self.handle_spawn(&ctx) {
                    Ok((msg, data)) => Ok(PluginResult::Broadcast(msg, Some(data))),
                    Err(e) => Ok(PluginResult::Reply(e, None)),
                },
                "list" => {
                    let (text, data) = self.handle_list();
                    Ok(PluginResult::Reply(text, Some(data)))
                }
                "stop" => match self.handle_stop(&ctx) {
                    Ok((msg, data)) => Ok(PluginResult::Broadcast(msg, Some(data))),
                    Err(e) => Ok(PluginResult::Reply(e, None)),
                },
                "logs" => match self.handle_logs(&ctx) {
                    Ok(msg) => Ok(PluginResult::Reply(msg, None)),
                    Err(e) => Ok(PluginResult::Reply(e, None)),
                },
                _ => Ok(PluginResult::Reply(
                    "unknown action. usage: /agent spawn|list|stop|logs".to_owned(),
                    None,
                )),
            }
        })
    }
}

/// Configure a [`Command`] to spawn in its own process group.
///
/// This ensures `/agent stop` can kill the entire process tree (ralph + all
/// child claude processes) with a single `kill(-pgid, SIGTERM)`.
#[cfg(unix)]
fn set_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // Safety: setsid() is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

/// No-op on non-Unix platforms.
#[cfg(not(unix))]
fn set_process_group(_cmd: &mut Command) {}

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

/// Send SIGTERM to a process group, wait `grace_secs`, then SIGKILL if the
/// leader is still alive.
///
/// Uses `kill(-pid, ...)` to signal the entire process group, ensuring child
/// processes (e.g. claude spawned by room-ralph) are also terminated.
fn stop_process(pid: u32, grace_secs: u64) {
    #[cfg(unix)]
    {
        // Negative PID signals the entire process group.
        let pgid = -(pid as i32);
        unsafe {
            libc::kill(pgid, libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_secs(grace_secs));
        if is_process_alive(pid) {
            unsafe {
                libc::kill(pgid, libc::SIGKILL);
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
        assert_eq!(plugin.handle_list().0, "no agents spawned");
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

        let (output, _data) = plugin.handle_list();
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
        let (msg, _data) = result.unwrap();
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
                assert!(values.contains(&"scout".to_owned()));
                assert!(values.contains(&"qa".to_owned()));
                assert!(values.contains(&"coordinator".to_owned()));
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
    fn spawn_personality_auto_name_skips_used() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        // Pre-insert agents with the first pool names to force later picks
        let coder = personalities::resolve_personality("coder")
            .unwrap()
            .unwrap();
        let first_name = format!("coder-{}", coder.naming.name_pool[0]);
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                first_name.clone(),
                SpawnedAgent {
                    username: first_name.clone(),
                    pid: std::process::id(),
                    model: "opus".to_owned(),
                    personality: "coder".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        // The name pool should skip the first name and pick the second
        let used: Vec<String> = {
            let agents = plugin.agents.lock().unwrap();
            agents.keys().cloned().collect()
        };
        let generated = coder.generate_username(&used);
        assert_ne!(generated, first_name);
        assert!(generated.starts_with("coder-"));
    }

    #[test]
    fn logs_missing_username() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let ctx = make_ctx(&plugin, vec!["logs"], vec![]);
        let result = plugin.handle_logs(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("usage"));
    }

    #[test]
    fn logs_unknown_agent() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let ctx = make_ctx(&plugin, vec!["logs", "nobody"], vec![]);
        let result = plugin.handle_logs(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no agent named"));
    }

    #[test]
    fn logs_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let log_path = dir.path().join("empty.log");
        fs::write(&log_path, "").unwrap();

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
                    log_path: log_path.clone(),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let ctx = make_ctx(&plugin, vec!["logs", "bot1"], vec![]);
        let result = plugin.handle_logs(&ctx).unwrap();
        assert!(result.contains("empty"));
    }

    #[test]
    fn logs_default_tail() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let log_path = dir.path().join("agent.log");

        // Write 30 lines
        let lines: Vec<String> = (1..=30).map(|i| format!("line {i}")).collect();
        fs::write(&log_path, lines.join("\n")).unwrap();

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
                    log_path: log_path.clone(),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let ctx = make_ctx(&plugin, vec!["logs", "bot1"], vec![]);
        let result = plugin.handle_logs(&ctx).unwrap();
        assert!(result.contains("last 20 of 30 lines"));
        assert!(result.contains("line 11"));
        assert!(result.contains("line 30"));
        assert!(!result.contains("line 10\n"));
    }

    #[test]
    fn logs_custom_tail() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let log_path = dir.path().join("agent.log");

        let lines: Vec<String> = (1..=10).map(|i| format!("line {i}")).collect();
        fs::write(&log_path, lines.join("\n")).unwrap();

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
                    log_path: log_path.clone(),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let ctx = make_ctx(&plugin, vec!["logs", "bot1", "--tail", "3"], vec![]);
        let result = plugin.handle_logs(&ctx).unwrap();
        assert!(result.contains("last 3 of 10 lines"));
        assert!(result.contains("line 8"));
        assert!(result.contains("line 10"));
        assert!(!result.contains("line 7\n"));
    }

    #[test]
    fn logs_tail_larger_than_file() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        let log_path = dir.path().join("agent.log");

        fs::write(&log_path, "only one line").unwrap();

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
                    log_path: log_path.clone(),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let ctx = make_ctx(&plugin, vec!["logs", "bot1", "--tail", "50"], vec![]);
        let result = plugin.handle_logs(&ctx).unwrap();
        assert!(result.contains("last 1 of 1 lines"));
        assert!(result.contains("only one line"));
    }

    #[test]
    fn logs_missing_log_file() {
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
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/nonexistent/path/agent.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let ctx = make_ctx(&plugin, vec!["logs", "bot1"], vec![]);
        let result = plugin.handle_logs(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot read log file"));
    }

    #[test]
    fn logs_invalid_tail_value() {
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
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let ctx = make_ctx(&plugin, vec!["logs", "bot1", "--tail", "abc"], vec![]);
        let result = plugin.handle_logs(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid --tail value"));
    }

    #[test]
    fn logs_zero_tail_rejected() {
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
                    personality: String::new(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let ctx = make_ctx(&plugin, vec!["logs", "bot1", "--tail", "0"], vec![]);
        let result = plugin.handle_logs(&ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("--tail must be at least 1"));
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
            PluginResult::Reply(msg, _) => assert!(msg.contains("unknown action")),
            PluginResult::Broadcast(..) => panic!("expected Reply, got Broadcast"),
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

        let (output, _data) = plugin.handle_list();
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

        let (output, _data) = plugin.handle_list();
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

        let (output, _data) = plugin.handle_list();
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

        let (output, _data) = plugin.handle_list();
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

        let (output, _data) = plugin.handle_list();
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

        let (output, _data) = plugin.handle_list();
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

        let (output, _data) = plugin.handle_list();
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

        let (output, _data) = plugin.handle_list();
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

    // ── structured data tests (#697) ─────────────────────────────────────

    #[test]
    fn list_data_contains_agents_array() {
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
                    personality: "coder".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: PathBuf::from("/tmp/test.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let (_text, data) = plugin.handle_list();
        assert_eq!(data["action"], "list");
        let agents = data["agents"].as_array().expect("agents should be array");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["username"], "bot1");
        assert_eq!(agents[0]["model"], "opus");
        assert_eq!(agents[0]["personality"], "coder");
        assert_eq!(agents[0]["status"], "running");
    }

    #[test]
    fn list_empty_data_has_empty_agents() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        let (_text, data) = plugin.handle_list();
        assert_eq!(data["action"], "list");
        let agents = data["agents"].as_array().expect("agents should be array");
        assert!(agents.is_empty());
    }

    #[test]
    fn stop_data_includes_action_and_username() {
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

        let ctx = make_ctx(&plugin, vec!["stop", "bot1"], vec![]);
        let (text, data) = plugin.handle_stop(&ctx).unwrap();
        assert!(text.contains("bot1"));
        assert_eq!(data["action"], "stop");
        assert_eq!(data["username"], "bot1");
        assert_eq!(data["was_alive"], false);
    }

    // ── ABI entry point tests ────────────────────────────────────────────

    #[test]
    fn abi_declaration_matches_plugin() {
        let decl = &ROOM_PLUGIN_DECLARATION;
        assert_eq!(decl.api_version, room_protocol::plugin::PLUGIN_API_VERSION);
        unsafe {
            assert_eq!(decl.name().unwrap(), "agent");
            assert_eq!(decl.version().unwrap(), env!("CARGO_PKG_VERSION"));
            assert_eq!(decl.min_protocol().unwrap(), "0.0.0");
        }
    }

    #[test]
    fn abi_create_with_empty_config() {
        let plugin_ptr = unsafe { room_plugin_create(std::ptr::null(), 0) };
        assert!(!plugin_ptr.is_null());
        let plugin = unsafe { Box::from_raw(plugin_ptr) };
        assert_eq!(plugin.name(), "agent");
        assert_eq!(plugin.version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn abi_create_with_json_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = format!(
            r#"{{"state_path":"{}","socket_path":"{}","log_dir":"{}"}}"#,
            dir.path().join("agents.json").display(),
            dir.path().join("room.sock").display(),
            dir.path().join("logs").display()
        );
        let plugin_ptr = unsafe { room_plugin_create(config.as_ptr(), config.len()) };
        assert!(!plugin_ptr.is_null());
        let plugin = unsafe { Box::from_raw(plugin_ptr) };
        assert_eq!(plugin.name(), "agent");
    }

    #[test]
    fn abi_destroy_frees_plugin() {
        let plugin_ptr = unsafe { room_plugin_create(std::ptr::null(), 0) };
        assert!(!plugin_ptr.is_null());
        unsafe { room_plugin_destroy(plugin_ptr) };
    }

    #[test]
    fn abi_destroy_null_is_safe() {
        unsafe { room_plugin_destroy(std::ptr::null_mut()) };
    }

    // ── HealthStatus tests ───────────────────────────────────────────────

    #[test]
    fn health_status_display_healthy() {
        assert_eq!(HealthStatus::Healthy.to_string(), "healthy");
    }

    #[test]
    fn health_status_display_stale() {
        assert_eq!(HealthStatus::Stale.to_string(), "stale");
    }

    #[test]
    fn health_status_display_exited_code() {
        assert_eq!(HealthStatus::Exited(Some(0)).to_string(), "exited (0)");
        assert_eq!(HealthStatus::Exited(Some(1)).to_string(), "exited (1)");
    }

    #[test]
    fn health_status_display_exited_signal() {
        assert_eq!(HealthStatus::Exited(None).to_string(), "exited (signal)");
    }

    #[test]
    fn health_status_equality() {
        assert_eq!(HealthStatus::Healthy, HealthStatus::Healthy);
        assert_eq!(HealthStatus::Stale, HealthStatus::Stale);
        assert_ne!(HealthStatus::Healthy, HealthStatus::Stale);
        assert_ne!(HealthStatus::Exited(Some(0)), HealthStatus::Exited(Some(1)));
        assert_eq!(HealthStatus::Exited(None), HealthStatus::Exited(None));
    }

    #[test]
    fn compute_health_exited_process() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        let agent = SpawnedAgent {
            username: "dead-bot".to_owned(),
            pid: 999_999_999, // non-existent PID
            model: "sonnet".to_owned(),
            personality: "coder".to_owned(),
            spawned_at: Utc::now() - chrono::Duration::minutes(10),
            log_path: dir.path().join("dead-bot.log"),
            room_id: "test-room".to_owned(),
        };

        let exit_codes = HashMap::new();
        let health = plugin.compute_health(&agent, &exit_codes, Utc::now());
        assert_eq!(health, HealthStatus::Exited(None));
    }

    #[test]
    fn compute_health_exited_with_code() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        let agent = SpawnedAgent {
            username: "dead-bot".to_owned(),
            pid: 999_999_999,
            model: "sonnet".to_owned(),
            personality: "coder".to_owned(),
            spawned_at: Utc::now() - chrono::Duration::minutes(10),
            log_path: dir.path().join("dead-bot.log"),
            room_id: "test-room".to_owned(),
        };

        let mut exit_codes = HashMap::new();
        exit_codes.insert("dead-bot".to_owned(), Some(1));
        let health = plugin.compute_health(&agent, &exit_codes, Utc::now());
        assert_eq!(health, HealthStatus::Exited(Some(1)));
    }

    #[test]
    fn on_message_updates_last_seen() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        // Insert a tracked agent.
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "tracked-bot".to_owned(),
                SpawnedAgent {
                    username: "tracked-bot".to_owned(),
                    pid: std::process::id(),
                    model: "sonnet".to_owned(),
                    personality: "coder".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: dir.path().join("bot.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        // Before any message, last_seen should be empty.
        assert!(plugin.last_seen_at.lock().unwrap().is_empty());

        // Simulate a message from the tracked agent.
        let msg = room_protocol::make_message("test-room", "tracked-bot", "hello");
        plugin.on_message(&msg);

        let last_seen = plugin.last_seen_at.lock().unwrap();
        assert!(last_seen.contains_key("tracked-bot"));
    }

    #[test]
    fn on_message_ignores_untracked_users() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        // No agents registered — message from random user should be ignored.
        let msg = room_protocol::make_message("test-room", "random-user", "hello");
        plugin.on_message(&msg);

        assert!(plugin.last_seen_at.lock().unwrap().is_empty());
    }

    #[test]
    fn stale_threshold_default_is_five_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());
        assert_eq!(plugin.stale_threshold_secs, 300);
    }

    #[test]
    fn health_stale_when_last_seen_exceeds_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let mut plugin = test_plugin(dir.path());
        plugin.stale_threshold_secs = 60; // 1 minute for test

        let agent = SpawnedAgent {
            username: "stale-bot".to_owned(),
            pid: std::process::id(), // current process = alive
            model: "sonnet".to_owned(),
            personality: "coder".to_owned(),
            spawned_at: Utc::now() - chrono::Duration::minutes(10),
            log_path: dir.path().join("stale-bot.log"),
            room_id: "test-room".to_owned(),
        };

        // Set last_seen to 2 minutes ago (exceeds 1 minute threshold).
        {
            let mut last_seen = plugin.last_seen_at.lock().unwrap();
            last_seen.insert(
                "stale-bot".to_owned(),
                Utc::now() - chrono::Duration::seconds(120),
            );
        }

        let exit_codes = HashMap::new();
        let health = plugin.compute_health(&agent, &exit_codes, Utc::now());
        assert_eq!(health, HealthStatus::Stale);
    }

    #[test]
    fn health_healthy_when_recently_seen() {
        let dir = tempfile::tempdir().unwrap();
        let mut plugin = test_plugin(dir.path());
        plugin.stale_threshold_secs = 60;

        let agent = SpawnedAgent {
            username: "active-bot".to_owned(),
            pid: std::process::id(),
            model: "sonnet".to_owned(),
            personality: "coder".to_owned(),
            spawned_at: Utc::now() - chrono::Duration::minutes(10),
            log_path: dir.path().join("active-bot.log"),
            room_id: "test-room".to_owned(),
        };

        // Set last_seen to 30 seconds ago (within 1 minute threshold).
        {
            let mut last_seen = plugin.last_seen_at.lock().unwrap();
            last_seen.insert(
                "active-bot".to_owned(),
                Utc::now() - chrono::Duration::seconds(30),
            );
        }

        let exit_codes = HashMap::new();
        let health = plugin.compute_health(&agent, &exit_codes, Utc::now());
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[test]
    fn health_healthy_when_never_seen_but_alive() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        let agent = SpawnedAgent {
            username: "new-bot".to_owned(),
            pid: std::process::id(), // current process = alive
            model: "sonnet".to_owned(),
            personality: "coder".to_owned(),
            spawned_at: Utc::now(),
            log_path: dir.path().join("new-bot.log"),
            room_id: "test-room".to_owned(),
        };

        let exit_codes = HashMap::new();
        let health = plugin.compute_health(&agent, &exit_codes, Utc::now());
        assert_eq!(health, HealthStatus::Healthy);
    }

    #[test]
    fn handle_list_includes_health_column() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = test_plugin(dir.path());

        // Insert an agent with the current PID so it shows as alive.
        {
            let mut agents = plugin.agents.lock().unwrap();
            agents.insert(
                "test-bot".to_owned(),
                SpawnedAgent {
                    username: "test-bot".to_owned(),
                    pid: std::process::id(),
                    model: "sonnet".to_owned(),
                    personality: "coder".to_owned(),
                    spawned_at: Utc::now(),
                    log_path: dir.path().join("test-bot.log"),
                    room_id: "test-room".to_owned(),
                },
            );
        }

        let (text, data) = plugin.handle_list();
        // Header should include health column.
        assert!(text.contains("health"));
        // Agent data should include health field.
        let agents = data["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["health"], "healthy");
    }
}
