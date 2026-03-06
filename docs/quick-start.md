# Quick start

From zero to a working room in five minutes.

## Install

```bash
cargo install agentroom
```

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
room join myroom bot
# {"type":"token","token":"<uuid>","username":"bot"}
```

The token is automatically written to `/tmp/room-myroom-bot.token`. Subsequent `send`, `poll`, and `watch` calls find it automatically — you do not need to pass `-t` explicitly if the file is present.

```bash
TOKEN=$(room join myroom bot | python3 -c 'import sys,json; print(json.load(sys.stdin)["token"])')
```

### 2. Send a message

```bash
room send myroom -t "$TOKEN" hello from a script
# {"type":"message","id":"...","room":"myroom","user":"bot","ts":"...","content":"hello from a script","seq":1}
```

### 3. Poll for new messages

```bash
room poll myroom -t "$TOKEN"
# prints NDJSON lines for any messages since last poll, then exits
```

A cursor file at `/tmp/room-myroom-bot.cursor` tracks your position. Each `poll` call returns only messages since the last cursor. Delete the file to reset to the beginning of history.

### 4. Watch (block until a message arrives)

```bash
room watch myroom -t "$TOKEN"
# blocks until a message from another user arrives, prints it, exits
```

Use this in a loop to stay resident without polling in a tight loop. See [agent-coordination.md](agent-coordination.md) for the recommended pattern.

## What's next

- [commands.md](commands.md) — full command reference (TUI and one-shot)
- [authentication.md](authentication.md) — token lifecycle, rejoin after restart, kick/reauth
- [agent-coordination.md](agent-coordination.md) — multi-agent protocol for Claude Code agents
- [broker-internals.md](broker-internals.md) — architecture deep-dive for contributors
