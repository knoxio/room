# room — Agent Coordination Guide

## What is `room`?

`room` is a CLI tool that lets multiple Claude agents and humans share a group chat over
Unix domain sockets. It is the coordination layer for this project. When you are working
on a feature, you are expected to join the shared room and stay connected for the duration
of your work.

## Joining

```bash
room <room-id> <your-username> --agent -n 20
```

- `<room-id>` — shared identifier for the session (you will be told this)
- `<your-username>` — use your branch name (e.g. `feat-status`, `feat-pm`, `feat-readme`)
- `--agent` — non-interactive JSON mode; messages arrive on stdout, send via stdin
- `-n 20` — replay the last 20 messages on join so you have context

## Wire format

Every line on stdout is a JSON object:

```json
{"type":"join","id":"...","room":"...","user":"alice","ts":"..."}
{"type":"message","id":"...","room":"...","user":"bob","ts":"...","content":"hello"}
{"type":"leave","id":"...","room":"...","user":"alice","ts":"..."}
{"type":"command","id":"...","room":"...","user":"bob","ts":"...","cmd":"claim","params":["task"]}
{"type":"reply","id":"...","room":"...","user":"bob","ts":"...","reply_to":"<msg-id>","content":"ack"}
```

To send a plain message, write a JSON line to stdin:

```json
{"type":"message","content":"your message here"}
```

To send a reply to a specific message (use the `id` field from the received message):

```json
{"type":"reply","reply_to":"<message-id>","content":"your reply"}
```

To send a structured command:

```json
{"type":"command","cmd":"claim","params":["describe what you are claiming"]}
```

Plain text (non-JSON) is also accepted and treated as a message.

## Expected behaviour

### On join
1. Read the replayed history to understand what has already been discussed or decided.
2. Announce yourself: who you are, what branch you are on, and what you intend to work on.
3. Wait briefly for acknowledgement or objections before starting work.

### During work
- **Announce intent before touching shared code.** If you are about to modify a file that
  another agent might also need to change, say so and wait for a reply.
- **Use `/claim <description>`** (as a command message) to declare ownership of a task or
  file. Others will see this and avoid conflicts.
- **Broadcast blockers.** If you are stuck or need a decision from another agent or the
  human, say so clearly. Do not silently stall.
- **Reply to specific messages** using the `reply` type with the original message's `id`.
  This keeps threads readable.
- **Check in periodically** — a short status update every few minutes is better than
  silence.

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
  main.rs      — CLI parsing, broker/client detection
  broker.rs    — Unix socket server, message fanout, persistence
  client.rs    — Connects to broker, runs TUI or agent mode
  tui.rs       — ratatui split-pane interface
  message.rs   — Wire format enum, constructors, parse_client_line
  history.rs   — NDJSON load/append
  lib.rs       — Re-exports all modules (required for integration tests)
tests/
  integration.rs — 14 integration tests against a live broker
```

Key invariants to preserve:
- **Broker is the sole writer** to the chat file. Never write to it from a client.
- **`Message` is a flat internally-tagged enum** — do not use `#[serde(flatten)]` with
  `#[serde(tag)]`, it breaks deserialization.
- **All file IO uses `std::fs` (synchronous) or explicit `spawn_blocking`** — `tokio::fs`
  wrappers get cancelled on runtime shutdown.
- **`src/lib.rs` must export any new modules** so integration tests can import them.
- **All tests must pass** before committing: `cargo test`.

## Running tests

```bash
cargo test
```

All 38 tests must remain green. Add tests for any new behaviour.
