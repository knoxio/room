# Design: `/agent` & `/spawn` Commands (#434)

## Problem

Spawning a `room-ralph` agent today requires shell access and manual CLI invocation:

```bash
room-ralph myroom bot-name --profile coder --issue 42
```

There is no way to spawn, inspect, or stop agents from within a room. The host must
SSH into the machine, find the right flags, and manage processes by hand. For a
multi-agent coordination tool, this is a gap — the room itself should be the control
plane for its agents.

## Goals

1. Let the host (or any authorized user) spawn a ralph agent from inside a room via
   `/agent` (fine-grained) or `/spawn` (pre-defined personality).
2. Track spawned agents by PID so the room can list, inspect, and stop them.
3. Clean up orphaned agent processes on broker shutdown.
4. Keep the crate boundary clean: room-cli shells out to `room-ralph` — it does not
   link ralph internals.

## Non-goals

- Agent-to-agent delegation (out of scope; coordination happens via room messages).
- Hot-reloading an agent's prompt or profile mid-run.
- Remote spawn across machines (daemon is single-host today).

## Terminology

- **personality** — a named preset that maps to a profile + prompt + model + tool
  restrictions. Stored in a registry, invoked by name with `/spawn`.
- **profile** — the `room-ralph` `Profile` enum (Coder, Reviewer, Coordinator, Notion,
  Reader). Controls tool auto-approval and hard-blocks.
- **agent process** — a running `room-ralph` child process, tracked by PID.

---

## Design

### 1. Spawn Interface

Two commands, both routed through the plugin system:

#### `/agent` — fine-grained spawn

```
/agent <username> [--profile <profile>] [--model <model>] [--issue <N>]
       [--prompt <text>] [--personality <file>] [--max-iter <N>]
       [--allow-tools <list>] [--disallow-tools <list>] [--allow-all]
```

Every flag maps 1:1 to a `room-ralph` CLI argument. The room ID is implicit (the
current room). The username must not collide with an existing online user or a
previously spawned agent that is still running.

Returns a system message:

```json
{"type":"system","content":"agent bot-42 spawned (pid 12345, profile: coder, model: opus)"}
```

#### `/spawn` — personality shortcut

```
/spawn <personality> [username]
```

Looks up `<personality>` in the personality registry (see section 4), expands it to
the equivalent `/agent` invocation, and spawns. If `username` is omitted, it defaults
to `<personality>-<short-uuid>` (e.g. `reviewer-a1b2`).

Example:

```
/spawn reviewer
→ equivalent to: /agent reviewer-a1b2 --profile reviewer --model sonnet --prompt "Review PRs..."
```

### 2. Spawn Mechanics

The spawn command is implemented as a **plugin** (`AgentPlugin`) registered with the
`PluginRegistry`. This keeps it out of the built-in command routing in `commands.rs`
and makes it optional — brokers that don't want agent spawning simply don't register
the plugin.

#### Process lifecycle

```
/agent or /spawn
  → AgentPlugin::handle()
    → validate params (username uniqueness, profile validity)
    → build Command::new("room-ralph") with args
    → set ROOM_SOCKET env var to current broker socket
    → spawn child process (detached, stdout/stderr to log file)
    → record SpawnedAgent { pid, username, profile, model, spawned_at, log_path }
    → broadcast system message to room
```

The plugin holds an `Arc<Mutex<HashMap<String, SpawnedAgent>>>` tracking all agents it
has spawned. This map is the source of truth for `/agent list` and `/agent stop`.

#### Detached spawn

The child process must outlive the plugin's `handle()` future. Use
`std::process::Command` with:

- `.stdin(Stdio::null())` — ralph reads from room, not stdin.
- `.stdout(Stdio::from(log_file))` — capture output for debugging.
- `.stderr(Stdio::from(log_file))` — same log file.
- No `.kill_on_drop()` — the process is intentionally detached.

Log files go to `~/.room/logs/agent-<username>-<timestamp>.log`.

#### Socket propagation

The spawned `room-ralph` needs to connect to the same broker. Two options:

- **Daemon mode:** Pass `--socket <daemon-socket-path>` so ralph connects to the
  daemon's UDS socket. The daemon routes ralph's `ROOM:` prefix to the correct room.
- **Single-room mode:** Pass `--socket <room-socket-path>`. Ralph connects directly
  to the room's per-room socket.

The plugin reads the socket path from `RoomState` (or `DaemonState` context) and
passes it through.

### 3. Agent Lifecycle Management

#### `/agent list`

```
/agent list
```

Returns a table of all spawned agents:

```
 username     | pid   | profile     | model  | uptime  | status
 reviewer-a1  | 12345 | reviewer    | sonnet | 14m     | running
 coder-b3     | 12400 | coder       | opus   | 3m      | running
 scout-c7     | 12380 | reader      | haiku  | 22m     | exited (0)
```

Status is determined by checking `kill(pid, 0)` (signal 0 = existence check). If the
process is gone, the exit code is read from `waitpid` if available, otherwise marked
as `exited (unknown)`.

#### `/agent stop <username>`

```
/agent stop reviewer-a1
```

Sends `SIGTERM` to the agent's PID. If the process doesn't exit within 5 seconds,
sends `SIGKILL`. Removes the entry from the spawn map. Broadcasts:

```json
{"type":"system","content":"agent reviewer-a1 stopped by host"}
```

Host-only by default. See section 6 for permission model.

#### `/agent logs <username> [--tail <N>]`

```
/agent logs reviewer-a1 --tail 50
```

Reads the last N lines (default 20) from the agent's log file and returns them as a
private reply to the invoker. Useful for debugging stuck agents without SSH.

#### Orphan cleanup

On broker shutdown (triggered by `/exit`, SIGTERM, or graceful close), the plugin's
`Drop` impl (or a shutdown hook) iterates all spawned agents and sends SIGTERM, then
SIGKILL after a grace period. This prevents orphaned ralph processes consuming
resources after the room is gone.

Implementation: the plugin registers a `tokio::sync::watch` subscriber for the
broker's shutdown signal (the existing `watch::channel<bool>` pattern). When shutdown
fires, it runs the cleanup loop.

On broker restart, the plugin loads the persisted PID map from
`~/.room/state/agents-<room>.json` and reaps any orphaned processes (SIGTERM → grace
→ SIGKILL). This prevents resource leaks from broker crashes. The PID file is written
on every spawn/stop mutation and deleted when the last agent exits or is stopped.

### 4. Personality Registry

Personalities are named presets stored as TOML files in a well-known directory:

```
~/.room/personalities/
  reviewer.toml
  coder.toml
  scout.toml
  qa.toml
```

#### Format

```toml
[personality]
name = "reviewer"
description = "Reviews PRs for correctness and style"
profile = "reviewer"
model = "sonnet"
max_iter = 20

[prompt]
text = """
You are a code reviewer. Focus on correctness, test coverage, and adherence to the
project's coding standards. Use `gh pr` commands to leave reviews.
"""

[tools]
allow = []
disallow = ["Write", "Edit"]
```

#### Resolution order

1. Check `~/.room/personalities/<name>.toml`
2. Check built-in defaults (compiled into the plugin)

Built-in defaults ship with the binary so `/spawn` works out of the box:

| Personality | Profile | Model | Description |
|---|---|---|---|
| `coder` | Coder | opus | Full dev agent — reads, writes, tests, commits |
| `reviewer` | Reviewer | sonnet | PR review — read-only code access, gh pr commands |
| `scout` | Reader | haiku | Codebase exploration — search and summarize, no writes |
| `qa` | Coder | sonnet | Test writer — focuses on test coverage gaps |
| `coordinator` | Coordinator | sonnet | BA/triage — reads code, manages issues, coordinates |

User-defined personalities in `~/.room/personalities/` override built-ins with the same
name.

#### Reconciliation with #439 (compiled-in defaults in room-ralph)

Issue #439 adds compiled-in personality defaults to `room-ralph` itself. The layering
is: room-ralph ships compiled-in defaults (used when ralph is invoked directly from
CLI), while the `/spawn` plugin reads TOML overrides from `~/.room/personalities/`.
When both exist for the same name, TOML wins. This means:

- `room-ralph myroom bot --profile reviewer` uses ralph's compiled-in Reviewer profile.
- `/spawn reviewer` checks TOML first, falls back to compiled-in defaults.
- Users can customize personalities without rebuilding the binary.

### 5. TUI Integration

#### Command palette

`/agent` and `/spawn` appear in the command palette like any plugin command. The
`AgentPlugin::commands()` method returns `CommandInfo` entries with typed parameter
schemas:

```rust
CommandInfo {
    name: "agent",
    params: vec![
        ParamSchema { name: "action", param_type: ParamType::Choice(vec![
            "spawn", "list", "stop", "logs"
        ]), required: true, .. },
        ParamSchema { name: "target", param_type: ParamType::Text, required: false, .. },
    ],
    ..
}

CommandInfo {
    name: "spawn",
    params: vec![
        ParamSchema { name: "personality", param_type: ParamType::Choice(
            personality_names()  // dynamic from registry
        ), required: true, .. },
        ParamSchema { name: "username", param_type: ParamType::Text, required: false, .. },
    ],
    ..
}
```

The TUI's `MentionPicker` already handles `ParamType::Choice` — personality names
will auto-complete.

#### Status panel

Spawned agents appear in the member status panel like any other user (they join the
room via ralph's normal `room join` + `room subscribe` flow). Their `/set_status`
updates are visible. No special TUI rendering is needed — agents are users.

### 6. Security & Permissions

#### Who can spawn?

By default, only the host can run `/agent` and `/spawn`. This prevents unprivileged
users from consuming machine resources by spawning arbitrary processes.

Future enhancement: an allowlist of usernames permitted to spawn agents, configurable
via room config or a `/agent allow <username>` admin command.

#### Resource limits

The plugin enforces:

- **Max concurrent agents per room**: configurable, default 5. Prevents a runaway
  `/spawn` loop from exhausting system resources.
- **Username uniqueness**: rejects spawn if the username is already taken (online user
  or running agent).
- **Model allowlist**: optionally restrict which models agents can use (e.g. prevent
  spawning opus agents on a resource-constrained host). Default: no restriction.

#### Audit trail

Every spawn, stop, and lifecycle event is broadcast as a system message to the room
and persisted in the chat history. The host (and any agent polling the room) can see
the full audit trail via `room query --all -s "agent"`.

Log files provide a detailed record of each agent's claude invocations, tool usage,
and room interactions.

#### `--allow-all` flag

`/agent <username> --allow-all` passes `--allow-all` through to `room-ralph`, which
bypasses all tool restrictions (no `--allowedTools`, no `--disallowedTools` sent to
claude). This gives the agent full unrestricted tool access in `-p` mode.

When `--allow-all` is set:
- `--profile`, `--allow-tools`, and `--disallow-tools` are silently ignored (ralph
  logs a warning at startup).
- The spawn system message includes `allow-all: true` so the audit trail is explicit.
- All tool invocations are logged to the agent's log file regardless.

This flag exists because some workflows (fast prototyping, trusted dev agents) need
unrestricted access without composing the right profile + tool overrides. The host
accepts the risk by explicitly passing the flag.

---

## Implementation Plan

### Phase 1: Spawn MVP (plugin + /agent + /agent list + /agent stop)

Files:
- `crates/room-cli/src/plugin/agent.rs` — NEW: `AgentPlugin` struct, spawn/list/stop
  handlers, `SpawnedAgent` tracking map, PID persistence to
  `~/.room/state/agents-<room>.json`, `--allow-all` passthrough
- `crates/room-cli/src/plugin/mod.rs` — register `AgentPlugin` in default plugin set
- `crates/room-cli/src/broker/mod.rs` — pass socket path to plugin context (extend
  `CommandContext` or `RoomMetadata` with socket info)
- `crates/room-cli/src/paths.rs` — add `agents_state_path()` helper

Tests:
- Unit tests for param validation, username collision checks, PID file round-trip
- Integration test: spawn a ralph process via plugin, verify it joins the room, stop it
- Unit test: `--allow-all` produces correct `Command` args (no --allowedTools/--disallowedTools)

### Phase 2: Personalities (/spawn + registry)

Files:
- `crates/room-cli/src/plugin/agent.rs` — add personality registry, TOML loading,
  built-in defaults, `/spawn` command handler
- `~/.room/personalities/*.toml` — user-defined personalities (not in repo)

Tests:
- Unit tests for TOML deserialization, override resolution, built-in defaults
- Integration test: `/spawn reviewer` creates a ralph with correct profile

### Phase 3: Lifecycle hardening

Files:
- `crates/room-cli/src/plugin/agent.rs` — orphan cleanup on shutdown, `/agent logs`,
  exit code tracking
- `crates/room-cli/src/paths.rs` — add `agent_log_dir()` path helper

Tests:
- Integration test: broker shutdown reaps spawned agents
- Unit test: log tail reads correct number of lines

### Phase 4: TUI autocomplete (depends on #436 typed command enums)

Files:
- `crates/room-cli/src/tui/widgets.rs` — personality name completion in palette
- `crates/room-cli/src/plugin/agent.rs` — dynamic `CommandInfo` with personality names

Tests:
- Unit test: palette filters personality names on keystroke

---

## Open Questions

1. **Should personalities be room-scoped or global?** Current proposal: global
   (`~/.room/personalities/`). Room-scoped would require storing TOML in the room's
   data directory, adding complexity for unclear benefit.

2. **Should `/agent stop` be graceful-only?** Current proposal: SIGTERM → 5s grace →
   SIGKILL. Alternative: only SIGTERM, let ralph handle its own shutdown. Risk: a
   stuck claude subprocess could keep ralph alive indefinitely.

3. **~~PID file persistence across restarts?~~** Resolved: persist to
   `~/.room/state/agents-<room>.json`. On restart, load and reap orphans. Consensus
   from r2d2 + ba: crash recovery is the real risk, not planned restarts.

4. **Should agents auto-subscribe to the room they're spawned in?** Ralph already
   does `room join` + `room subscribe` in its loop startup. The spawn plugin just
   needs to ensure the room ID is passed correctly. No extra work needed unless we
   want the plugin to pre-subscribe on the agent's behalf (race condition risk).

---

## Alternatives Considered

### A. Built-in command (not plugin)

Add `/agent` to `RESERVED_COMMANDS` and handle in `commands.rs` directly. Rejected
because:
- Spawning processes is heavyweight side-effect logic that doesn't belong in the
  command router.
- Making it a plugin keeps it optional — test brokers and lightweight deployments
  can skip it.
- The plugin system already provides `CommandInfo`, `CommandContext`, and lifecycle
  hooks — no need to reinvent them.

### B. room-ralph links room-cli

Have ralph import broker types directly instead of shelling out. Rejected because:
- Violates the invariant: "room-ralph is a CLIENT — it must NOT link room-cli
  transport or broker code."
- Would create a circular dependency risk (room-cli plugin spawns ralph, ralph
  imports room-cli).
- Shelling out to `room-ralph` is simpler, tested, and already works.

### C. Daemon manages agents (not plugin)

Add spawn/stop methods to `DaemonState` directly. Rejected because:
- Couples agent management to daemon mode — single-room brokers couldn't spawn agents.
- `DaemonState` is already complex (room lifecycle, UDS dispatch, UserRegistry).
  Adding PID tracking would push it over the complexity threshold.
- A plugin can work in both daemon and single-room mode by reading the socket path
  from context.
