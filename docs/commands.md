# Commands

`room` has two sets of commands: **CLI subcommands** (run from your shell) and
**in-room slash commands** (typed inside an active session).

---

## CLI subcommands

### `room join`

```
room join <username>
```

Register a username with the broker and receive a global session token. If no broker
is running, one is started automatically.

Prints a JSON object: `{"token":"<uuid>","username":"<name>"}` and exits.
The token is also written to `~/.room/state/room-<username>.token`.

The token is global (not per-room). Use `room subscribe <room-id>` to join
specific rooms after obtaining a token.

Returns an error if the username is already in use.

---

### `room send`

```
room send <room-id> --token <token> [--to <user>] <message...>
```

Send a message to a room and exit. All arguments after `--token` are joined
as the message content.

| Flag | Description |
|------|-------------|
| `--token`, `-t` | Session token from `room join` (required) |
| `--to <user>` | Send as a direct message to `<user>` only |

Prints the broadcast message JSON and exits.

---

### `room query`

```
room query [<room-id>] --token <token> [OPTIONS]
```

Unified query engine for message history and real-time polling. Without flags,
returns all messages (newest-first). `room poll` and `room watch` are aliases.

| Flag | Description |
|------|-------------|
| `--token`, `-t` | Session token from `room join` (required) |
| `-r, --room <rooms>` | Filter by room IDs (comma-separated) |
| `--all` | Query all daemon-managed rooms |
| `--new` | Only messages since last poll (advances cursor) |
| `--wait` | Block until a new message arrives (implies `--new`) |
| `--user <name>` | Filter by sender |
| `-s, --search <text>` | Substring content search (case-sensitive) |
| `--regex <pattern>` | Regex content search |
| `-m, --mentions-only` | Only messages that @mention you |
| `--from <room:seq>` | After this position (exclusive) |
| `--to <room:seq>` | Before this position (inclusive) |
| `--since <ISO8601>` | After this timestamp |
| `--until <ISO8601>` | Before this timestamp |
| `-n <count>` | Limit output to N messages |
| `--asc` / `--desc` | Sort order (default: asc for `--new`, desc for history) |
| `-p, --public` | Bypass subscription filter (requires another filter) |
| `--id <room:seq>` | Look up a single message by ID |
| `--interval <secs>` | Poll interval for `--wait` (default: 5) |

**Room resolution:** When no room is specified, `--new` and `--wait` auto-discover
all daemon rooms. `--all` explicitly opts into all rooms. Bare `room query` without
a room source errors.

**Subscription filtering:** Messages are filtered by your per-room subscription tier
(Full, MentionsOnly, Unsubscribed) unless `-p` is used. `-p` requires at least one
other narrowing filter.

---

### `room poll`

```
room poll [<room-id>] --token <token> [--since <message-id>] [--rooms r1,r2]
```

Alias for `room query --new`. Fetch new messages since your last poll and exit.
When no room is specified, auto-discovers all daemon rooms. Subscription tiers
are respected per room.

| Flag | Description |
|------|-------------|
| `--token`, `-t` | Session token from `room join` (required) |
| `--since <id>` | Return messages after this ID, ignoring stored cursor |
| `--rooms <r1,r2>` | Poll multiple rooms (comma-separated) |
| `--mentions-only` | Only messages that @mention you |

---

### `room pull`

```
room pull <room-id> --token <token> [-n <count>]
```

Fetch the last N messages from history without updating the poll cursor.
Reads the chat file directly — no broker connection required. Useful for
re-reading recent context after a context reset.

| Flag | Description |
|------|-------------|
| `--token`, `-t` | Session token from `room join` (required) |
| `-n <count>` | Number of messages to return (default: 20, max: 200) |

---

### `room watch`

```
room watch [<room-id>] --token <token> [--interval <secs>] [--rooms r1,r2]
```

Alias for `room query --new --wait`. Block until at least one message from
another user arrives, then print it as NDJSON and exit. When no room is
specified, auto-discovers all daemon rooms and watches your full stream.
Subscription tiers are respected per room.

| Flag | Description |
|------|-------------|
| `--token`, `-t` | Session token from `room join` (required) |
| `--interval <secs>` | Poll interval in seconds (default: 5) |
| `--rooms <r1,r2>` | Watch multiple rooms (comma-separated) |

---

### `room who`

```
room who <room-id> --token <token>
```

Query online members and their statuses. Returns a system message listing
all connected users. The response format matches `/who` from the TUI.

| Flag | Description |
|------|-------------|
| `--token`, `-t` | Session token from `room join` (required) |

---

### `room <room-id> <username>` (TUI mode)

```
room <room-id> <username> [-n <count>] [-f <chat-file>] [--agent]
```

Start the interactive TUI, joining an existing room or becoming the broker if
none is running.

| Flag | Description |
|------|-------------|
| `-n <count>` | History messages to replay on join (default: 20) |
| `-f <chat-file>` | Custom chat file path (only used when creating a new room) |
| `--agent` | Non-interactive agent mode: read JSON from stdin, write JSON to stdout |

---

## In-room slash commands

These commands are typed in the message input and sent to the broker. In the
TUI, press `/` to open the command palette and browse available commands.

### `/dm <user> <message>`

Send a private message to `<user>`. The message is delivered to the sender,
the recipient, and the room host (for moderation). Other users do not see it.

```
/dm alice Hey, are you available?
```

---

### `/reply <id> <message>`

Reply to a specific message by its ID. The reply is broadcast to the room with
a `reply_to` reference so clients can thread it visually.

```
/reply abc123 agreed, let's do that
```

---

### `/info [username]`

Inspect room metadata or a specific user. Without arguments, returns room-level
information (visibility, config, host, member/subscriber counts). With a username
argument, returns that user's online status, status text, subscription tier, and
host flag. The response is private (only you see it).

```
/info
/info bob
/info @alice
```

---

### `/set_status <status>`

Set your presence status (e.g. `away`, `coding`, `brb`). The status appears
next to your name in `/who` output. An empty value clears the status.

```
/set_status reviewing PRs
/set_status
```

---

### `/who`

Request the list of currently connected users. The broker responds privately
(only you see the result) with a system message listing usernames and their
statuses.

```
/who
```

Example response: `online — alice, bob: away, charlie`

---

## Plugin commands

Plugin commands are provided by compiled-in plugins registered in the
`PluginRegistry`. They are dispatched via `dispatch_plugin` in the broker.

### `/queue <action> [args]`

Manage a persistent task backlog. Items are stored in an NDJSON file alongside
the chat file.

| Action | Description |
|--------|-------------|
| `add <description>` | Add a task to the backlog. Broadcasts to the room. |
| `list` | List all queued tasks with their index. Private reply. |
| `remove <index>` | Remove a task by its 1-based index. Broadcasts to the room. |
| `pop` | Claim and remove the first task from the queue. Broadcasts to the room. |

```
/queue add implement caching layer
/queue list
/queue remove 2
/queue pop
```

---

### `/taskboard <action> [args]`

Manage task lifecycle with claim ownership, plan submission, approval gates,
and lease-based expiry. Tasks are stored in an NDJSON file alongside the chat
file. Each claimed task has a 10-minute lease TTL — expired leases auto-release
the task back to open status.

| Action | Description |
|--------|-------------|
| `post <description>` | Create a new task. Broadcasts to the room. |
| `list` | List all tasks with status and assignee. Private reply. |
| `show <task-id>` | Show full detail for a task (status, description, poster, assignee, plan, approved_by, notes, lease elapsed). Private reply. |
| `claim <task-id>` | Claim an open task. Only open tasks can be claimed. Broadcasts to the room. |
| `assign <task-id> <username>` | Assign an open task to a user. Only the task poster or room host can assign. Sets status to claimed. Broadcasts to the room. |
| `plan <task-id> <plan-text>` | Submit or resubmit a plan for a claimed task. Only the assignee can submit. Broadcasts the plan text to the room for review. |
| `approve <task-id>` | Approve a planned task. Only the task poster or room host can approve. Broadcasts to the room. |
| `update <task-id> [notes]` | Update progress notes and renew the lease. Only the assignee can update. Warns if the task is not yet approved. |
| `release <task-id>` | Release a task back to open status. Only the assignee or room host can release. Broadcasts to the room. |
| `finish <task-id>` | Mark a task as finished. Only the assignee can finish. Broadcasts to the room. |
| `cancel <task-id> [reason]` | Cancel a task. Poster, assignee, or host can cancel. Cannot cancel finished tasks. Optional reason is included in the broadcast. |

```
/taskboard post implement caching layer
/taskboard list
/taskboard show tb-001
/taskboard claim tb-001
/taskboard plan tb-001 add LRU cache in broker state with 5m TTL
/taskboard approve tb-001
/taskboard update tb-001 cache struct done, writing tests
/taskboard release tb-001
/taskboard finish tb-001
/taskboard assign tb-001 saphire
/taskboard cancel tb-001 scope changed
```

---

## Admin commands

Admin commands are restricted to the **room host** — the first user to join
the room (who also started the broker). Other users receive a
`permission denied` error.

### `/kick <user>`

Invalidate `<user>`'s session token and disconnect them. The username is
reserved; the user cannot rejoin until `/reauth` is issued.

```
/kick spammer
```

---

### `/reauth <user>`

Remove the kick restriction on `<user>`, allowing them to `room join` again
with a new token and re-subscribe to rooms.

```
/reauth alice
```

---

### `/clear-tokens`

Revoke all session tokens for this room. Every user (including the host) must
run `room join` again to obtain a new token and re-subscribe to rooms. Existing TUI sessions are
disconnected.

---

### `/exit`

Broadcast a shutdown notice and terminate the broker process. All connected
clients are disconnected. The chat history file is preserved.

---

### `/clear`

Truncate the room's chat history file to zero bytes and broadcast a notice.
Previously sent messages are permanently deleted.
