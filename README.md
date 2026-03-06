# room

`room` is a multi-user group chat tool for Unix systems. It lets humans and AI agents share a persistent chat room over a Unix domain socket. One process acts as the broker; everyone else connects as a client. The broker fans out messages to all connected clients and appends the full history to an NDJSON file on disk.

## Installation

```bash
cargo install agentroom
```

The installed binary is named `room`.

For feature deep-dives, see the **[docs/](docs/)** folder.

## Claude Code plugin

A Claude Code plugin teaches Claude when and how to use `room send` and `room poll` automatically, and adds explicit slash commands.

**Plugin contents:**

| Component | Name | Purpose |
|-----------|------|---------|
| Skill | `room` | Auto-triggers coordination behaviour — polls on session start, announces intent, broadcasts progress |
| Command | `/room:check` | Explicitly poll for new messages |
| Command | `/room:send <message>` | Explicitly send a message to the room |

**Install:**

```bash
claude plugin install github:knoxio/room
```

Once installed, Claude will automatically follow the coordination protocol in any project whose `CLAUDE.md` documents a room ID.

## Multi-agent coordination

`room` was designed as a coordination layer for multiple Claude Code agents working on the same codebase. The full agent coordination protocol — how agents announce intent, claim files, poll for conflicts, and hand off work — is documented in [`CLAUDE.md`](./CLAUDE.md).

**To adopt this pattern in your own project:** copy [`CLAUDE.md`](./CLAUDE.md) into your project root, update the codebase overview section, and point agents to your room ID. Each agent will follow the protocol automatically.

## Quick start

The first invocation of `room <room-id> <username>` in a given room starts the broker automatically. Subsequent invocations in other terminals (or on other processes) connect as clients.

```
# Terminal 1 — starts the broker and connects
room myroom alice

# Terminal 2 — joins the existing room
room myroom bob
```

## CLI reference

### Join a session (required before send/poll/watch)

```
room join <room-id> <username>
```

Registers your username with the broker and receives a session token. Writes the token to `/tmp/room-<room-id>-<username>.token`. Run this once per broker lifetime — the token persists so subsequent `send`, `poll`, and `watch` calls find it automatically. Returns an error if the username is already taken in the room.

```bash
room join myroom bot
# {"type":"token","token":"<uuid>","username":"bot"}
# Token written to /tmp/room-myroom-bot.token
```

### Connect to a room (TUI)

```
room <room-id> <username> [OPTIONS]
```

Opens a full-screen terminal UI. This is the standard human-facing entry point. The first invocation in a room also starts the broker.

| Argument / flag | Description |
|---|---|
| `<room-id>` | Identifier for the room. Used to name the socket (`/tmp/room-<id>.sock`) and the default chat file (`/tmp/<id>.chat`). |
| `<username>` | Your display name in the room. |
| `-n <N>` | Number of history messages to replay on join. Default: `20`. |
| `-f <path>` | Path to the chat file. Only used when creating a new room; ignored by clients that connect to an existing broker. |
| `--agent` | Non-interactive agent mode. Reads JSON from stdin, writes JSON to stdout. See [Agent mode](#agent-mode). |

### One-shot send

```
room send --token <token> <room-id> [<message>...]
```

Connects to a running broker, delivers one message, prints the broadcast JSON to stdout, and exits. Requires a broker to be running and a valid session token from `room join`. All tokens after `<room-id>` are joined into the message content.

```bash
room send --token <uuid> myroom hello from a script
# {"type":"message","id":"...","room":"myroom","user":"bot","ts":"...","content":"hello from a script"}
```

| Flag | Description |
|---|---|
| `-t, --token <token>` | Session token from `room join` (required) |
| `--to <username>` | Send as a direct message to this user only |

The printed JSON is the authoritative broadcast record — use its `id` as a `--since` cursor for `room poll`.

### Poll for new messages

```
room poll --token <token> <room-id> [--since <id>]
```

Reads the chat file directly (no socket, no broker required) and prints unseen messages as NDJSON to stdout, then exits. A per-user cursor file at `/tmp/room-<id>-<username>.cursor` tracks the last seen message ID so subsequent calls return only new content.

| Flag | Description |
|---|---|
| `-t, --token <token>` | Session token from `room join` (required) |
| `--since <id>` | Return only messages after this message ID. Overrides the stored cursor for this call. |

```bash
# First call: prints all messages, writes cursor
room poll --token <uuid> myroom

# Second call: prints only messages since the cursor (nothing if up to date)
room poll --token <uuid> myroom

# Jump to a specific position
room poll --token <uuid> myroom --since "b5b6becb-..."
```

The cursor file is at `/tmp/room-<id>-<username>.cursor`. Delete it to reset to the beginning of history.

### Watch for new messages

```
room watch --token <token> <room-id> [--interval <N>]
```

Polls the chat file on a configurable interval and blocks until at least one message from another user arrives, then prints those messages as NDJSON and exits. Shares the cursor file with `room poll` — no messages are re-delivered between the two commands.

| Flag | Description |
|---|---|
| `-t, --token <token>` | Session token from `room join` (required) |
| `--interval <N>` | Poll interval in seconds. Default: `5`. |

Use this instead of a manual polling loop. See [Autonomous loop](#autonomous-loop-claude-code--sequential-tool-model) for the recommended pattern.

## TUI

Without `--agent`, `room` opens a full-screen terminal UI built with [ratatui](https://github.com/ratatui-org/ratatui).

```
+------------------------------------------+
|                  room                    |
| 10:01:02  alice joined                   |
| 10:01:05  alice: hey everyone            |
| 10:01:10  bob joined                     |
| 10:01:12  bob: hi alice                  |
|                                          |
+------------------------------------------+
|  alice                                   |
|  hello world_                            |
+------------------------------------------+
```

**Key bindings:**

| Key | Action |
|---|---|
| `Enter` | Send the current input |
| `Shift+Enter` / `\` + `Enter` | Insert a newline (multi-line message) |
| `Ctrl-C` | Quit |
| `Up` / `Down` | Scroll message history one line |
| `PageUp` / `PageDown` | Scroll ten lines |
| `Backspace` | Delete last character |

**Sending commands from the TUI:**

Prefix your input with `/` to send a command instead of a plain message:

```
/claim implement the auth module
/set_status reviewing PRs
/who
/dm bob hey, can we sync?
```

The command and its arguments are sent as a `command` message (see [Wire format](#wire-format)).

**Admin commands (TUI only, slash prefix):**

Admin commands use the same `/` prefix as user commands and are available in the command palette:

| Command | Description |
|---|---|
| `/kick <username>` | Invalidates the user's token — they cannot send further messages. Their username remains reserved; use `/reauth` to let them rejoin. |
| `/reauth <username>` | Clears the user's token entry so they can `room join` again with the same username. |
| `/clear-tokens` | Clears all tokens for the room — every user must `room join` again. |
| `/exit` | Broadcasts a shutdown notice and stops the broker. |
| `/clear` | Truncates the chat history file and broadcasts a notice. |

## Agent mode

> **For agents that cannot block on a persistent connection** (e.g. Claude Code, which uses sequential tool calls), use `room join` + [`room send`](#one-shot-send), [`room poll`](#poll-for-new-messages), and [`room watch`](#watch-for-new-messages) instead. They are stateless, exit immediately, and compose cleanly with tool-calling workflows.

Pass `--agent` to run without a TUI. This is designed for long-lived automated processes that can maintain a persistent socket connection.

- **Stdout:** every event from the broker is printed as a JSON object, one per line.
- **Stdin:** send messages by writing JSON objects (or plain text) to stdin, one per line.

### Staying connected

The agent process stays alive until the broker closes the connection. To keep stdin open (and therefore keep the socket write-half alive), use a persistent stdin holder:

```bash
mkfifo /tmp/room-in

# Start the room agent (blocks until a writer opens the FIFO)
room myroom myagent --agent -n 20 < /tmp/room-in > /tmp/room-out.log &

# Hold the write end open so the agent never sees EOF on stdin
tail -f /dev/null > /tmp/room-in &

# Send a message
echo '{"type":"message","content":"hello"}' > /tmp/room-in

# Read new output
tail -f /tmp/room-out.log
```

### Autonomous loop (Claude Code / sequential tool model)

For agents that need to stay resident all day without human re-prompting, use `room watch` with `run_in_background` and `TaskOutput`:

```
1. room join <room-id> <username>        # once per broker lifetime
2. room watch --token <uuid> <room-id>   # run_in_background=true, timeout=600000
3. Block on TaskOutput — exits when a foreign message arrives
4. Act on the message
5. room send --token <uuid> <room-id> "..."
6. Go back to step 2 — re-launch room watch to resume listening
```

The cursor is shared between `room poll` and `room watch` automatically — no deduplication needed.

### History replay

On join, the broker sends the full chat history followed by your own `join` event. The agent buffers all events until it sees its own join, then prints the last `-n` entries and streams all subsequent events in real time.

### Sending from stdin

Write one JSON object per line. Plain text lines are also accepted and treated as plain messages.

```jsonc
// Plain message
{"type":"message","content":"hello room"}

// Reply to a specific message (use the id from a received event)
{"type":"reply","reply_to":"<message-id>","content":"ack"}

// Structured command
{"type":"command","cmd":"claim","params":["describe what you are claiming"]}

// Plain text (also works)
hey everyone
```

The broker assigns the `id`, `room`, `user`, and `ts` fields — you do not send them.

## Wire format

Every line on stdout (and in the chat file) is a JSON object with a `type` field. All events share a common envelope:

| Field | Type | Description |
|---|---|---|
| `type` | string | Event type (see below) |
| `id` | string | UUID v4, assigned by the broker |
| `room` | string | Room identifier |
| `user` | string | Username of the sender or subject |
| `ts` | string | ISO 8601 timestamp (UTC) |

### `join`

Emitted when a user connects to the room.

```json
{"type":"join","id":"10a9f010-...","room":"myroom","user":"alice","ts":"2026-03-05T10:00:00Z"}
```

### `leave`

Emitted when a user disconnects.

```json
{"type":"leave","id":"ab1e7e97-...","room":"myroom","user":"alice","ts":"2026-03-05T10:01:00Z"}
```

### `message`

A plain chat message.

```json
{"type":"message","id":"b5b6becb-...","room":"myroom","user":"alice","ts":"2026-03-05T10:01:05Z","content":"hello everyone"}
```

| Field | Description |
|---|---|
| `content` | Message body |

### `reply`

A message addressed to a specific earlier message.

```json
{"type":"reply","id":"c3d4e5f6-...","room":"myroom","user":"bob","ts":"2026-03-05T10:01:10Z","reply_to":"b5b6becb-...","content":"hey alice"}
```

| Field | Description |
|---|---|
| `reply_to` | `id` of the message being replied to |
| `content` | Reply body |

### `command`

A structured command. The broker may act on it (e.g. for built-in commands) or broadcast it to all clients for application-level handling.

```json
{"type":"command","id":"d4e5f6a7-...","room":"myroom","user":"alice","ts":"2026-03-05T10:01:15Z","cmd":"claim","params":["auth module"]}
```

| Field | Description |
|---|---|
| `cmd` | Command name |
| `params` | Array of string arguments |

### `system`

A message generated by the broker itself, not by a user. Used for server-side responses such as the output of `/who`.

```json
{"type":"system","id":"e5f6a7b8-...","room":"myroom","user":"broker","ts":"2026-03-05T10:01:20Z","content":"alice: online, bob: online"}
```

| Field | Description |
|---|---|
| `content` | System message body |

### `dm`

A private message delivered only to the recipient, the sender, and the broker host.

```json
{"type":"dm","id":"c3d4e5f6-...","room":"myroom","user":"alice","ts":"2026-03-05T10:01:10Z","to":"bob","content":"hey, can we sync?"}
```

| Field | Description |
|---|---|
| `to` | Username of the intended recipient |
| `content` | Message body |

## Chat history

The broker appends every event to an NDJSON file (one JSON object per line). The default path is `/tmp/<room-id>.chat`. Override it with `-f <path>` when starting a new room.

On join, the broker replays the full history to the new client before broadcasting the join event. Use `-n <N>` to control how many recent messages are shown (default: 20).

The broker is the **sole writer** to the chat file. Clients must never write to it directly.

## Architecture

```
room <room-id> <username>           # TUI / agent mode
  |
  +-- no socket found?  --> start Broker  --> listen on /tmp/room-<id>.sock
  |                                            append to /tmp/<id>.chat
  |
  +-- socket found?     --> connect as Client (TUI or --agent)

room join <room-id> <username>      # one-shot: get a session token
  |
  +-- connect to socket --> handshake --> broker issues UUID token
                        <-- token JSON
                        --> writes /tmp/room-<id>-<username>.token
                        --> disconnect

room send --token <uuid> <room-id>  # one-shot: authenticated send
  |
  +-- connect to socket --> TOKEN:<uuid> handshake --> broker resolves identity
                        --> send message --> broker broadcasts & persists
                        <-- echo JSON (the broadcast record)
                        --> disconnect

room poll --token <uuid> <room-id>  # one-shot poll (no socket)
  |
  +-- read /tmp/<id>.chat directly
  +-- filter by cursor / --since
  +-- print NDJSON, update /tmp/room-<id>-<username>.cursor

room watch --token <uuid> <room-id> # blocking poll (no socket)
  |
  +-- loop: read /tmp/<id>.chat, filter foreign messages
  +-- sleep --interval seconds, repeat until foreign message found
  +-- print NDJSON, update cursor, exit
```

The broker accepts connections over a Unix domain socket. Each client gets a dedicated broadcast receiver. When the broker receives a message from one client, it persists it to disk and fans it out to all connected clients via a `tokio::broadcast` channel.

`room join` issues a session token that identifies the user for all subsequent one-shot operations. `room send` and `room poll` use a lightweight token-authenticated handshake — no join/leave events are emitted. `room poll` and `room watch` are entirely broker-free (read the chat file directly) and safe to call from multiple processes simultaneously.

## User status

Users can set a status string on themselves and query who is currently online.

Status is stored in broker memory and cleared automatically when a user disconnects. It is not persisted to the chat file.

### Commands

**TUI:**

```
/set_status working on auth
/set_status
/who
```

**Agent mode:**

```json
{"type":"command","cmd":"set_status","params":["working on auth"]}
{"type":"command","cmd":"set_status","params":[]}
{"type":"command","cmd":"who","params":[]}
```

**One-shot (send a command via `room send`):**

```bash
room send --token <uuid> myroom '{"type":"command","cmd":"who","params":[]}'
```

### Behaviour

- `/set_status <message>` — sets your status string and broadcasts a `system` message to all connected clients announcing the change. Pass no arguments to clear your status.
- `/who` — returns a `system` message listing all connected users and their current statuses. The response is sent only to the requesting client; it is not broadcast.

Both commands use the existing `command` input type. Responses are delivered as `system` messages.

## Direct messages

Users can send private messages that are delivered only to the recipient, the sender, and the broker host. DMs are always written to the chat history file for auditing, but bystanders never receive them over the wire.

The **broker host** is the first user to connect to a room (i.e. the user who started the broker process). The host always receives a copy of every DM regardless of who the parties are.

### Sending a DM

**TUI:**

```
/dm bob hey, can we sync?
```

**One-shot:**

```bash
room send --token <uuid> myroom --to bob hey, can we sync?
```

**Agent mode:**

```json
{"type":"dm","to":"bob","content":"hey, can we sync?"}
```

### Behaviour

- The DM is delivered to: the **recipient**, the **sender**, and the **broker host**.
- All other connected users do not receive it.
- The message is persisted to the chat history file regardless of whether the recipient is currently online.
- If the recipient is offline, the sender still receives an echo of the DM (confirming it was saved).
