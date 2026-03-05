# room — Agent Coordination Guide

This file documents how AI agents should use `room` to coordinate with each other and with humans during parallel work on this repository.

## What is `room`?

`room` is a CLI tool that provides a shared group chat over Unix domain sockets. One process acts as the broker; all others connect as clients. The full message history is persisted to an NDJSON file on disk and replayed to new participants on join.

## Sending and receiving messages

Use the one-shot subcommands. They connect, act, and exit immediately — no persistent process required.

```bash
# Send a message
room send <room-id> <username> your message here

# Check for new messages since last poll
room poll <room-id> <username>

# Check messages since a specific message ID
room poll <room-id> <username> --since <id>
```

The `room poll` cursor is stored at `/tmp/room-<id>-<username>.cursor`. Subsequent calls with no `--since` return only messages that arrived after the previous poll.

## Coordination protocol

### On starting work

1. Poll for context: `room poll <room-id> <username>`
2. Announce intent: `room send <room-id> <username> "starting work on <task>"`
3. Wait for acknowledgement or objections before proceeding.

### During work

- **Poll before touching shared files**: `room poll <room-id> <username>`
- **Claim work before starting it**: `room send <room-id> <username> "/claim <description>"`
- **Announce blockers immediately.** Do not silently stall.
- **Broadcast progress at natural milestones** — silence is harder to coordinate around than noise.

### On completion

```bash
room send <room-id> <username> "done. changed: <file list>. <summary of decisions/tradeoffs>"
```

### Coordination rules

- **One agent per file at a time.** Ask before modifying a file someone else has claimed.
- **Schema/wire format changes require consensus.** Announce the proposed change, wait for agreement.
- **The human (host) has final say.** If the host sends a message, stop, acknowledge, follow the instruction.
- **Do not push or merge without announcing it** in the room first.

## Wire format

Every message is a JSON object with a `type` field. Key types:

| `type` | Meaning |
|--------|---------|
| `join` | User connected |
| `leave` | User disconnected |
| `message` | Plain chat message (`content` field) |
| `command` | Structured command (`cmd`, `params` fields) |
| `reply` | Reply to a specific message (`reply_to`, `content` fields) |
| `system` | Broker-generated notice |
| `dm` | Private message to one user |

All events carry `id` (UUID), `room`, `user`, and `ts` (ISO 8601 UTC).

## Environment

- Socket: `/tmp/room-<id>.sock`
- Chat history: `/tmp/<id>.chat` (NDJSON, broker is sole writer)
- Meta (chat path for broker recovery): `/tmp/room-<id>.meta`
- Poll cursor: `/tmp/room-<id>-<username>.cursor`
