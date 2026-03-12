# PRD: `/agent` Plugin — Spawn, Stop, List, Logs

## Status

Draft — 2026-03-12

## Problem

Spawning a room-ralph agent requires shell access and manual CLI invocation. There is
no way to spawn, inspect, or stop agents from within a room. The host must SSH into
the machine, find the right flags, and manage processes by hand. For a multi-agent
coordination tool, this is a gap — the room itself should be the control plane for
its agents.

## Goals

1. Let the host spawn a ralph agent from inside a room via `/agent spawn`.
2. Let the host use `/spawn <personality>` as a shortcut for pre-defined personalities.
3. Track spawned agents by PID so the room can list, inspect, and stop them.
4. Provision agents correctly: mint token, subscribe to room, create workdir, pass
   credentials via environment variable.
5. Clean up orphaned agent processes on broker shutdown.
6. Keep the crate boundary clean: room-cli shells out to `room-ralph` — it does not
   link ralph internals.

## Non-goals

- Agent-to-agent delegation (coordination happens via room messages).
- Hot-reloading an agent's prompt or personality mid-run.
- Remote spawn across machines (daemon is single-host today).
- Resource management beyond simple concurrent limits (Hive handles quotas).

## Design

### Command Interface

#### `/agent spawn` — fine-grained spawn

```
/agent spawn <username> [--personality <name>] [--model <model>] [--issue <N>]
             [--prompt <text>] [--max-iter <N>]
             [--allow-tools <list>] [--disallow-tools <list>] [--allow-all]
```

Spawns a room-ralph process with the specified configuration. If `--personality` is
given, loads the personality and uses its settings as defaults (explicit flags override).

Returns:
```json
{"type":"system","content":"agent dev-anna spawned (pid 12345, personality: coder, model: opus)"}
```

#### `/spawn` — personality shortcut

```
/spawn <personality> [--name <username>]
```

Looks up `<personality>` in the personality registry, auto-generates a username from
the name pool (or uses `--name` if provided), and spawns. Equivalent to `/agent spawn`
with the personality's full configuration.

Example:
```
/spawn reviewer
→ agent reviewer-kai spawned (pid 12400, personality: reviewer, model: sonnet)
```

#### `/agent list`

```
/agent list
```

Returns a table of all spawned agents:

```
 username     | pid   | personality | model  | uptime  | status
 dev-anna     | 12345 | coder       | opus   | 14m     | running
 reviewer-kai | 12400 | reviewer    | sonnet | 3m      | running
 scout-c7     | 12380 | scout       | haiku  | 22m     | exited (0)
```

Status is determined by `kill(pid, 0)` (signal 0 = existence check). Exited agents
show their exit code.

#### `/agent stop <username>`

```
/agent stop dev-anna
```

Sends SIGTERM to the agent's PID. If the process doesn't exit within 5 seconds,
sends SIGKILL. Removes the entry from the spawn map. Broadcasts:

```json
{"type":"system","content":"agent dev-anna stopped by host"}
```

#### `/agent logs <username> [--tail <N>]`

```
/agent logs reviewer-kai --tail 50
```

Reads the last N lines (default 20) from the agent's log file and returns them as a
system message. Useful for debugging stuck agents without SSH.

### Agent Provisioning

When `/agent spawn` or `/spawn` is invoked, the plugin performs these steps in order:

1. **Validate**: Check username uniqueness (no collision with online users or running
   agents). Check concurrent agent limit (default 5 per room).

2. **Mint token**: Call `room join <username>` to register the agent and get a token.
   The plugin runs this programmatically through the broker's token issuance, not by
   shelling out.

3. **Subscribe to room**: Call `room subscribe <room-id>` with Full tier so the agent
   receives all messages in the room.

4. **Create workdir**: Create `/tmp/room-agent-<username>/` as the agent's working
   directory. This is ephemeral — a host reboot clears it. The agent can clone repos,
   create scratch files, etc. here.

5. **Spawn process**: Build `Command::new("room-ralph")` with:
   - Positional args: `<room-id> <username>`
   - `--model`, `--issue`, `--max-iter`, `--prompt`, `--personality`, tool flags
     as configured by the personality or explicit flags
   - `--socket <broker-socket-path>` for broker connection
   - Environment: `ROOM_TOKEN=<minted-token-uuid>`
   - Working directory: `/tmp/room-agent-<username>/`
   - stdin: null, stdout/stderr: log file at `~/.room/logs/agent-<username>-<timestamp>.log`
   - Detached (no kill_on_drop)

6. **Record**: Store `SpawnedAgent { pid, username, personality, model, spawned_at,
   log_path, lifecycle }` in the plugin's tracking map.

7. **Broadcast**: Send system message to the room announcing the spawn.

### Token Delivery via ROOM_TOKEN

The spawner sets `ROOM_TOKEN=<uuid>` in the child process environment. room-ralph
checks for this variable at startup:

- If `ROOM_TOKEN` is set: use it directly, skip `room join` and `room subscribe`
  (the spawner already did both).
- If `ROOM_TOKEN` is not set: existing behavior — ralph calls `room join` and
  `room subscribe` itself.

This means ralph can be spawned both by the `/agent` plugin (pre-provisioned token)
and directly from CLI (self-provisioning). No breaking change.

### Socket Propagation

The spawned ralph needs to connect to the same broker:

- **Daemon mode**: Pass `--socket <daemon-socket-path>`. Ralph connects to the
  daemon's UDS socket, which routes to the correct room.
- **Single-room mode**: Pass `--socket <room-socket-path>`. Ralph connects directly.

The plugin reads the socket path from `RoomState` (or `DaemonState` context).

### Orphan Cleanup

**On broker shutdown** (via `/exit`, SIGTERM, or graceful close):
- The plugin iterates all spawned agents and sends SIGTERM.
- After a 5-second grace period, sends SIGKILL to any remaining.
- Implementation: register a `tokio::sync::watch` subscriber for the broker's
  shutdown signal.

**On broker restart**:
- The plugin loads the PID map from `~/.room/state/agents-<room>.json`.
- For each PID, check if the process is still alive via `kill(pid, 0)`.
- Reap any orphaned processes (SIGTERM then SIGKILL).
- Delete the PID file when the last agent is cleaned up.

**PID persistence**:
- Written to `~/.room/state/agents-<room>.json` on every spawn/stop.
- Format: `[{ "pid": 12345, "username": "dev-anna", "personality": "coder", ... }]`

### Security and Permissions

**Who can spawn?** Host-only by default. The host is the user who started the broker.
Future enhancement: an allowlist via `/agent allow <username>`.

**Resource limits:**
- Max concurrent agents per room: configurable, default 5.
- Username uniqueness: rejects spawn if username is taken.
- Model allowlist: optionally restrict which models can be used.

**Audit trail:** Every spawn, stop, and lifecycle event is broadcast as a system
message and persisted in chat history. Log files record claude invocations and tool
usage per agent.

**`--allow-all` flag:** Passes through to room-ralph, bypassing all tool restrictions.
The spawn system message includes `allow-all: true` for audit visibility. When set,
`--allow-tools` and `--disallow-tools` are ignored.

### TUI Integration

`/agent` and `/spawn` appear in the command palette via `AgentPlugin::commands()`.
The `ChoicePicker` widget provides autocomplete for:
- `/agent` action parameter: `spawn`, `list`, `stop`, `logs`
- `/spawn` personality parameter: dynamic list from personality registry

Spawned agents appear in the member status panel like any other user (they join via
ralph's normal flow). Their `/set_status` updates are visible. No special TUI
rendering needed — agents are users.

### Triple Interface (CLI + Slash + WS)

Per joao's directive, the plugin definition generates three interfaces:

1. **Slash command**: `/agent spawn dev-anna --personality coder` — routed through
   the broker's plugin system.
2. **CLI**: `room agent spawn dev-anna --personality coder` — oneshot subcommand
   that sends the equivalent command envelope over UDS/WS.
3. **WS/REST**: `POST /api/<room>/command` with body
   `{"plugin":"agent","action":"spawn","params":{"username":"dev-anna","personality":"coder"}}`
   — routed to the same handler.

The plugin trait stays pure: it receives a `CommandContext` and returns a response.
The routing layer (broker, CLI parser, REST handler) each deserialize their transport
format into `CommandContext` and call the same `handle()`.

### Structured Responses

Plugin responses include both human-readable `content` and machine-readable `data`:

```json
{
  "type": "system",
  "content": "agent dev-anna spawned (pid 12345, personality: coder, model: opus)",
  "plugin": "agent",
  "data": {
    "action": "spawn",
    "username": "dev-anna",
    "pid": 12345,
    "personality": "coder",
    "model": "opus"
  }
}
```

The `content` field is displayed in chat. The `data` field is for programmatic
consumers (Hive, scripts, other agents). This pattern applies to all plugin responses.

### Hive Integration

`/agent` is the room-native control plane for standalone usage. When Hive is present:

- Hive manages agent lifecycle through its own layer, not `/agent`.
- Hive calls `room join` and `room subscribe` directly via WS/REST.
- Hive spawns ralph processes through its own agent runner (which may handle repo
  cloning, workdir setup, and monitoring).
- **`/agent list` only shows plugin-spawned agents.** Hive-spawned agents are not
  tracked in the plugin's `SpawnedAgent` map because Hive provisions them independently
  (its own token minting, workdir setup, process management). They still appear as
  regular users in the member panel and `/who` output — they just aren't visible to
  `/agent list`, `/agent stop`, or `/agent health`.
- `/agent` should document this boundary: "when Hive is absent, this plugin is the
  agent control plane. When Hive is present, Hive manages agent lifecycle."

## Implementation Plan

### Phase 1: Spawn MVP

Files:
- `crates/room-cli/src/plugin/agent.rs` — NEW: `AgentPlugin` struct, spawn/list/stop
  handlers, `SpawnedAgent` tracking, PID persistence, `--allow-all` passthrough
- `crates/room-cli/src/plugin/mod.rs` — register `AgentPlugin`
- `crates/room-cli/src/broker/mod.rs` — expose socket path in `CommandContext`
- `crates/room-cli/src/paths.rs` — add `agents_state_path()`, `agent_log_dir()`
- `crates/room-ralph/src/room.rs` — check `ROOM_TOKEN` env var, skip join/subscribe
  if set

Tests:
- Unit: param validation, username collision, PID file round-trip
- Unit: `--allow-all` produces correct Command args
- Integration: spawn ralph via plugin, verify it joins the room, stop it

### Phase 2: Personalities and /spawn

Files:
- `crates/room-cli/src/plugin/agent.rs` — personality registry, TOML loading,
  built-in defaults, `/spawn` handler
- `crates/room-ralph/src/personalities.rs` — extend with full personality schema

Tests:
- Unit: TOML deserialization, override resolution, built-in defaults
- Unit: name pool draw-without-replacement, UUID fallback
- Integration: `/spawn reviewer` creates ralph with correct configuration

### Phase 3: Lifecycle hardening

Files:
- `crates/room-cli/src/plugin/agent.rs` — orphan cleanup on shutdown, `/agent logs`,
  exit code tracking, structured response `data` field

Tests:
- Integration: broker shutdown reaps spawned agents
- Unit: log tail reads correct number of lines
- Unit: structured response includes data field

### Phase 4: TUI autocomplete

Files:
- `crates/room-cli/src/tui/widgets.rs` — personality name completion in palette
- `crates/room-cli/src/plugin/agent.rs` — dynamic `CommandInfo` with personality names

Tests:
- Unit: palette filters personality names on keystroke

## Decided Questions

1. **`/agent stop` uses SIGTERM then SIGKILL.** SIGTERM with a 5-second grace period,
   then SIGKILL. SIGTERM-only is insufficient — a stuck claude subprocess can keep
   ralph alive indefinitely, and the host needs a guaranteed way to reclaim resources.

2. **Progress files use persistent storage.** Location:
   `~/.room/state/progress/<username>-<issue>.md`. Progress must survive both context
   exhaustion and host reboot. The ephemeral workdir (`/tmp/room-agent-<username>/`)
   is for scratch files only.
