# Commands

`room` has two sets of commands: **CLI subcommands** (run from your shell) and
**in-room slash commands** (typed inside an active session).

---

## CLI subcommands

### `room join`

```
room join <room-id> <username>
```

Register a username with the broker and receive a session token. If no broker
is running for the room, one is started automatically.

Prints a JSON object: `{"token":"<uuid>","username":"<name>"}` and exits.
The token is also written to `/tmp/room-<room-id>-<username>.token`.

Returns an error if the username is already in use in the room.

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

### `room poll`

```
room poll <room-id> --token <token> [--since <message-id>]
```

Fetch new messages since your last poll and exit. Prints NDJSON to stdout.
Updates a cursor file (`/tmp/room-<id>-<username>.cursor`) so subsequent
calls return only unseen messages.

| Flag | Description |
|------|-------------|
| `--token`, `-t` | Session token from `room join` (required) |
| `--since <id>` | Return messages after this ID, ignoring stored cursor |

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
room watch <room-id> --token <token> [--interval <secs>]
```

Block until at least one message from another user arrives, then print it as
NDJSON and exit. Shares the cursor file with `room poll` so no messages are
re-delivered. Useful for agents that need to wake on incoming messages.

| Flag | Description |
|------|-------------|
| `--token`, `-t` | Session token from `room join` (required) |
| `--interval <secs>` | Poll interval in seconds (default: 5) |

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

### `/claim <task>`

Register a task claim. The broker stores the claim in memory and broadcasts
a system message to all users. Each user can hold at most one claim at a
time — a new `/claim` replaces any existing claim.

```
/claim implement the /dm command
```

---

### `/unclaim`

Release your current task claim. Broadcasts a system message confirming the
release. No-op message if you have no active claim.

```
/unclaim
```

---

### `/claimed`

List all active task claims across all users. The response is sent privately
(only you see it). Useful for checking what tasks are taken before starting
work.

```
/claimed
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
with a new token.

```
/reauth alice
```

---

### `/clear-tokens`

Revoke all session tokens for this room. Every user (including the host) must
run `room join` again to obtain a new token. Existing TUI sessions are
disconnected.

---

### `/exit`

Broadcast a shutdown notice and terminate the broker process. All connected
clients are disconnected. The chat history file is preserved.

---

### `/clear`

Truncate the room's chat history file to zero bytes and broadcast a notice.
Previously sent messages are permanently deleted.
