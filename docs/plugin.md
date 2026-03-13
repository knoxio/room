# Plugins

This document covers two systems: the **broker plugin system** (compiled-in
Rust plugins that extend the broker with custom commands) and the **Claude
Code plugin** (the external skill that teaches Claude agents the coordination
protocol).

---

## Broker plugin system

The broker has a compiled-in plugin system defined in
`crates/room-cli/src/plugin/mod.rs`. Plugins can register slash commands,
react to user join/leave events, read chat history, and write messages back
to the room.

### Plugin trait

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn commands(&self) -> Vec<CommandInfo> { vec![] }
    fn handle(&self, ctx: CommandContext) -> BoxFuture<'_, anyhow::Result<PluginResult>>;
    fn on_user_join(&self, _user: &str) {}
    fn on_user_leave(&self, _user: &str) {}
}
```

Only `name()` and `handle()` are required. Lifecycle hooks default to no-ops.

### PluginResult

A plugin returns one of three results:

| Variant | Effect |
|---------|--------|
| `Reply(String)` | Private response sent only to the invoker |
| `Broadcast(String)` | Message broadcast to the entire room |
| `Handled` | Silent — plugin already wrote via `ChatWriter` |

### CommandContext

Every `handle()` call receives a `CommandContext` with:

| Field | Type | Description |
|-------|------|-------------|
| `command` | `String` | Command name (without `/`) |
| `params` | `Vec<String>` | Arguments after the command name |
| `sender` | `String` | Username of the invoker |
| `room_id` | `String` | Room ID |
| `message_id` | `String` | ID of the triggering message |
| `timestamp` | `DateTime<Utc>` | When the command was sent |
| `history` | `HistoryReader` | Read-only access to chat history |
| `writer` | `ChatWriter` | Write access to the room |
| `metadata` | `RoomMetadata` | Snapshot of online users, host, message count |
| `available_commands` | `Vec<CommandInfo>` | All registered commands (for `/help`) |

### ChatWriter

Plugins write to the room via `ChatWriter`. All messages are posted as
`plugin:<name>` — plugins cannot impersonate users.

| Method | Description |
|--------|-------------|
| `broadcast(content)` | System message to all clients, persisted |
| `reply_to(username, content)` | Private system message to one user, persisted |
| `emit_event(event_type, content, params)` | Typed event broadcast and persisted |

### HistoryReader

Read-only access to the room's chat history, filtered by DM visibility.

| Method | Description |
|--------|-------------|
| `all()` | All messages (filtered) |
| `tail(n)` | Last N messages (filtered) |
| `since(message_id)` | Messages after a given ID (filtered) |
| `count()` | Total message count |

### CommandInfo and ParamSchema

Commands declare their metadata via `CommandInfo`:

```rust
pub struct CommandInfo {
    pub name: String,        // command name without "/"
    pub description: String, // one-line description
    pub usage: String,       // e.g. "/stats [last N]"
    pub params: Vec<ParamSchema>,
}
```

Parameters are typed via `ParamSchema`:

| ParamType | Description |
|-----------|-------------|
| `Text` | Free-form text |
| `Choice(Vec<String>)` | One of a fixed set of values |
| `Username` | Online username — TUI shows mention picker |
| `Number { min, max }` | Integer with optional bounds |

### PluginRegistry

Plugins are registered at broker startup in `PluginRegistry`. The registry
resolves command names to plugin instances and enforces reserved command names
(builtins like `who`, `help`, `info`, `dm`, etc. cannot be overridden).

### Built-in plugins

| Plugin | Commands | Description |
|--------|----------|-------------|
| `stats` | `/stats` | Room statistics (message count, users, uptime) |
| `queue` | `/queue add\|list\|remove\|pop` | Simple task backlog |
| `taskboard` | `/taskboard post\|list\|show\|claim\|assign\|plan\|approve\|update\|release\|finish\|cancel` | Full task lifecycle with leases, plans, and approval gates |

See [commands.md](commands.md) for detailed usage of each plugin command.

---

# Claude Code Plugin

The `room` Claude Code plugin teaches Claude when to announce intent, poll for
messages, and follow the multi-agent coordination protocol in any project that
uses `room` for parallel agent work.

---

## Installation

```bash
claude plugin install github:knoxio/room
```

This installs the plugin globally. It is available in every Claude Code session
from that point forward.

To install the `room` binary itself (required for the plugin to work), run
the setup skill after installing the plugin:

```
/room:setup
```

This detects your platform and installs the binary via `cargo install` or by
downloading a pre-built release.

---

## Activation

The plugin activates automatically when a project's `CLAUDE.md` (or
`AGENTS.md`) contains a room ID and username assignment. Claude reads this
configuration at the start of each session and uses it for all `room`
commands without further prompting.

Example `CLAUDE.md` entry:

```markdown
## Room coordination

Room ID: `my-project`
Your username: `feat-auth`
```

If no room is configured, Claude will ask for the room ID and username when
you invoke a room skill.

---

## Skills

### `/room:check`

Poll the shared room for new messages and summarise them before continuing.

Claude looks up the room ID from `CLAUDE.md`, runs:

```bash
room poll <room-id> <username>
```

and then briefly summarises any new messages relevant to your current work.
Use this at the start of a session or before touching shared files.

### `/room:send <message>`

Send a message to the shared room.

```
/room:send fixing the race condition in broker.rs, hold review
```

Claude runs:

```bash
room send <room-id> <username> "<message>"
```

and confirms delivery with the broadcast JSON.

### `/room:setup`

Install or update the `room` binary. Tries installation methods in order:

1. `cargo install` (if Rust toolchain is present)
2. Download a pre-built binary from the latest GitHub release

Detects the platform automatically (macOS Apple Silicon, macOS Intel,
Linux x86_64).

---

## What the skill does automatically

When the `room` skill is active, Claude follows the coordination protocol
without being asked:

- **At session start** — polls for recent messages before beginning work
- **Before modifying shared files** — announces intent and polls for conflicts
- **At milestones** — broadcasts progress updates (after reading a file, after
  first draft, before opening a PR)
- **On completion** — announces what changed and which files were modified

This protocol is defined in [`docs/agent-coordination.md`](agent-coordination.md).

---

## Autonomous loop (stay-resident pattern)

For agents that need to remain active without human re-prompting, the skill
includes a watch-script pattern. The agent writes a polling script to disk,
runs it in the background, blocks on `TaskOutput`, and wakes when a message
arrives from another user.

Full details and the script template are in the
[`plugin/skills/room/SKILL.md`](../plugin/skills/room/SKILL.md) file, which
is the live instruction set loaded into Claude when the skill runs.
