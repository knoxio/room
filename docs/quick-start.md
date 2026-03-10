# Quick start

From zero to a working room in five minutes.

## Install

```bash
cargo install room-cli
```

> **Migrating from `agentroom`?** Run `cargo uninstall agentroom && cargo install room-cli`.
> The binary name (`room`) is unchanged.

The binary is named `room`. Verify with:

```bash
room --version
```

## Start a room (TUI)

Open two terminals. In the first:

```bash
room myroom alice
```

This starts the broker (if one is not already running for `myroom`) and opens the full-screen TUI. You are `alice`.

In the second terminal:

```bash
room myroom bob
```

You are now `bob`, connected to the same room. Type a message and press `Enter` to send. It appears in both windows.

> **How it works:** the first `room myroom alice` call looks for `/tmp/room-myroom.sock`. Finding none, it starts a broker listening on that socket and connects as a TUI client. `room myroom bob` finds the socket and connects directly.

## Key bindings

| Key | Action |
|---|---|
| `Enter` | Send message |
| `Shift+Enter` | Insert newline (multi-line message) |
| `Up` / `Down` | Scroll history one line |
| `PageUp` / `PageDown` | Scroll ten lines |
| `Ctrl-C` | Quit |

Type `/` to open the command palette.

## Send a command

From the TUI input, prefix with `/`:

```
/who
/set_status reviewing PRs
/dm bob hey, can we sync?
```

See [commands.md](commands.md) for the full reference.

## One-shot usage (scripts and agents)

For scripts or AI agents that need to send and poll without a persistent connection:

### 1. Register a session token (once per broker lifetime)

```bash
room join bot
# {"type":"token","token":"<uuid>","username":"bot"}
```

The token is also written to `~/.room/state/room-bot.token`. Save it to a variable — you must pass it explicitly with `-t` on every subsequent command:

```bash
TOKEN=$(room join bot | python3 -c 'import sys,json; print(json.load(sys.stdin)["token"])')
```

### 2. Subscribe to a room

```bash
room subscribe myroom -t "$TOKEN"
```

### 3. Send a message

```bash
room send myroom -t "$TOKEN" "hello from a script"
# {"type":"message","id":"...","room":"myroom","user":"bot","ts":"...","content":"hello from a script","seq":1}
```

### 4. Poll for new messages

```bash
# Poll a specific room
room poll myroom -t "$TOKEN"

# Poll all subscribed rooms (auto-discovers daemon rooms)
room poll -t "$TOKEN"
```

A cursor file at `~/.room/state/room-myroom-bot.cursor` tracks your position. Each `poll` call returns only messages since the last cursor. Delete the file to reset to the beginning of history.

### 5. Watch (block until a message arrives)

```bash
# Watch all subscribed rooms
room watch -t "$TOKEN"

# Watch a specific room
room watch myroom -t "$TOKEN"
```

### 6. Query with filters

```bash
# Search messages containing "bug" from alice
room query -t "$TOKEN" --all -s "bug" --user alice

# Last 10 messages from a specific room
room query myroom -t "$TOKEN" -n 10
```

Use `room watch` in a loop to stay resident without polling in a tight loop. See [agent-coordination.md](agent-coordination.md) for the recommended pattern.

## What's next

- [commands.md](commands.md) — full command reference (TUI and one-shot)
- [authentication.md](authentication.md) — token lifecycle, rejoin after restart, kick/reauth
- [agent-coordination.md](agent-coordination.md) — multi-agent protocol for Claude Code agents
- [broker-internals.md](broker-internals.md) — architecture deep-dive for contributors
