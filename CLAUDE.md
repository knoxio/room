# room — Agent Coordination Guide

## What is `room`?

`room` is a CLI tool that lets multiple Claude agents and humans share a group chat over
Unix domain sockets. It is the coordination layer for this project. When you are working
on a feature, you are expected to join the shared room and stay connected for the duration
of your work.

## Communicating from Claude Code (sequential tool model)

Claude Code cannot block on a persistent socket connection. Use the one-shot subcommands:

```bash
# Send a message — prints the broadcast JSON and exits
room send <room-id> <your-username> your message here

# Check for new messages since last poll — prints NDJSON and exits
room poll <room-id> <your-username>

# Check messages since a specific ID (overrides stored cursor)
room poll <room-id> <your-username> --since <message-id>
```

The cursor is stored at `/tmp/room-<id>-<username>.cursor`. A second `room poll` with no `--since` returns only messages that arrived after the first call.

### Staying resident (autonomous loop)

To remain active without human re-prompting, write a watch script to disk with the `Write` tool (not a heredoc — `$()` command substitution is blocked in some hook environments), then run it with `run_in_background=true` and `timeout=600000`:

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOM="<room-id>"
ME="<username>"
while true; do
  room poll "$ROOM" "$ME" > /tmp/room_msgs.txt 2>&1
  if grep -v "\"user\":\"$ME\"" /tmp/room_msgs.txt | grep -q "\"type\":\"message\""; then
    grep -v "\"user\":\"$ME\"" /tmp/room_msgs.txt | grep "\"type\":\"message\""
    break
  fi
  sleep 5
done
```

Block on `TaskOutput` — when a message arrives the task completes, you act, send a reply via `room send`, then re-launch the script. The self-message filter (`grep -v`) prevents the agent from waking on its own sends. Filtering for `"type":"message"` prevents waking on join/leave/system noise.

### Typical loop

```bash
# On starting work — announce yourself
room send myroom feat-myfeature "starting work on X"

# After any significant step — broadcast progress
room send myroom feat-myfeature "finished Y, moving on to Z"

# Before touching a shared file — check for conflicts
room poll myroom feat-myfeature
room send myroom feat-myfeature "about to modify src/broker.rs"

# On completion
room send myroom feat-myfeature "done. modified: src/broker.rs, tests/integration.rs"
```

## Persistent agent mode (long-lived processes only)

If your process can maintain a blocking connection (scripts, daemons), use `--agent`:

```bash
room <room-id> <your-username> --agent -n 20
```

Every event from the broker arrives as a JSON line on stdout. Send messages by writing JSON to stdin. This mode is **not suitable for Claude Code** — use `send`/`poll` instead.

## Wire format

Every message is a JSON object with a `type` field:

```json
{"type":"join","id":"...","room":"...","user":"alice","ts":"..."}
{"type":"message","id":"...","room":"...","user":"bob","ts":"...","content":"hello"}
{"type":"leave","id":"...","room":"...","user":"alice","ts":"..."}
{"type":"command","id":"...","room":"...","user":"bob","ts":"...","cmd":"claim","params":["task"]}
{"type":"reply","id":"...","room":"...","user":"bob","ts":"...","reply_to":"<msg-id>","content":"ack"}
```

To send structured input via `--agent` stdin or `room send`, plain text is also accepted.

## Expected behaviour

### On starting work
1. Poll for recent history: `room poll <room-id> <username>`
2. Announce yourself: who you are, what branch you are on, and what you intend to work on.
3. Wait briefly for acknowledgement or objections before starting work.

### During work
- **Announce intent before touching shared code.** If you are about to modify a file that
  another agent might also need to change, say so and wait for a reply.
- **Claim tasks before starting them**: `room send <room-id> <username> /claim <description>`
- **Broadcast blockers.** If you are stuck or need a decision, say so clearly. Do not silently stall.
- **Poll before significant steps** to catch instructions or conflicts you may have missed:
  `room poll <room-id> <username>`
- **Check in periodically** — a short status update every few steps is better than silence.

### Coordination rules
- **One agent per file at a time.** If you need to modify a file that another agent has
  claimed, ask them first.
- **Schema changes need consensus.** If your feature requires adding a new message type or
  changing the wire format, announce the proposed change and wait for agreement before
  implementing.
- **The human (host) has final say.** If the host sends a message, treat it as a directive.
  Stop what you are doing, acknowledge it, and follow the instruction.
- **Do not merge or push without announcing it** in the room first.

### On completion
1. Announce that your work is done and summarise what changed.
2. State which files were modified.
3. Note any decisions or trade-offs the other agents or the human should be aware of.

## Codebase overview

```
src/
  main.rs      — CLI parsing, subcommand dispatch (join / send / poll)
  broker.rs    — Unix socket server, message fanout, persistence
  client.rs    — Connects to broker, runs TUI or agent mode
  tui.rs       — ratatui split-pane interface
  message.rs   — Wire format enum, constructors, parse_client_line
  history.rs   — NDJSON load/append
  oneshot.rs   — send_message / poll_messages (no persistent connection)
  lib.rs       — Re-exports all modules (required for integration tests)
tests/
  integration.rs — 27 integration tests against a live broker
```

Key invariants to preserve:
- **Broker is the sole writer** to the chat file. Never write to it from a client.
- **`Message` is a flat internally-tagged enum** — do not use `#[serde(flatten)]` with
  `#[serde(tag)]`, it breaks deserialization.
- **All file IO uses `std::fs` (synchronous) or explicit `spawn_blocking`** — `tokio::fs`
  wrappers get cancelled on runtime shutdown.
- **`src/lib.rs` must export any new modules** so integration tests can import them.
- **All tests must pass** before committing: `cargo test`.

## Pre-push checklist

Run all three in order before every `git push`. CI enforces all of them and will fail if any step is skipped.

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
```

- `cargo fmt` — reformats code; commit the result if it changed anything.
- `cargo clippy -- -D warnings` — treats all warnings as errors; fix the root cause, never suppress.
- `cargo test` — all tests must pass.

## Running tests

```bash
cargo test
```

All tests must remain green. Add tests for any new behaviour.
