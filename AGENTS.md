# room — Agent Coordination Guide

This file documents how AI agents should use `room` to coordinate with each other and with humans during parallel work on this repository.

## What is `room`?

`room` is a CLI tool that provides a shared group chat over Unix domain sockets (and optionally WebSocket/REST). One process acts as the broker; all others connect as clients. The full message history is persisted to an NDJSON file on disk and replayed to new participants on join.

## Session setup

All one-shot commands require a session token. Register once per broker lifetime:

```bash
room join <username>
# {"type":"token","token":"<uuid>","username":"<username>"}
```

The token is global (not per-room). Use `room subscribe <room-id>` to join specific rooms. Pass the token explicitly with `-t` on every subsequent command — it is not auto-read from disk. Tokens persist across broker restarts.

## Sending and receiving messages

Use the one-shot subcommands. They connect, act, and exit immediately — no persistent process required.

```bash
# Send a message
room send <room-id> -t <token> your message here

# Send a direct message
room send <room-id> -t <token> --to <recipient> your message here

# Check for new messages since last poll
room poll <room-id> -t <token>

# Check messages since a specific message ID
room poll <room-id> -t <token> --since <id>

# Block until a foreign message arrives
room watch <room-id> -t <token> --interval 5

# Query online members and statuses
room who <room-id> -t <token>
```

The `room poll` cursor is stored at `/tmp/room-<id>-<username>.cursor`. Subsequent calls with no `--since` return only messages that arrived after the previous poll.

## Coordination protocol

### On starting work

1. Get a token if you don't have one: `room join <username>`, then `room subscribe <room-id>`
2. Poll for context: `room poll <room-id> -t <token>`
3. Announce intent: `room send <room-id> -t <token> "starting work on <task>"`
4. Wait for acknowledgement or objections before proceeding.

### During work

- **Claim tasks before starting**: `room send <room-id> -t <token> /claim <description>`
- **Poll before touching shared files**: `room poll <room-id> -t <token>`
- **Announce blockers immediately.** Do not silently stall.
- **Broadcast progress at natural milestones** — silence is harder to coordinate around than noise.
- **Set status at every milestone**: `room send <room-id> -t <token> /set_status <what you are doing>`

### On completion

```bash
room send <room-id> -t <token> "done. changed: <file list>. <summary of decisions/tradeoffs>"
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

All events carry `id` (UUID), `room`, `user`, `ts` (ISO 8601 UTC), and `seq` (monotonic sequence number).

## Environment

- Socket: `/tmp/room-<id>.sock`
- Chat history: `/tmp/<id>.chat` (NDJSON, broker is sole writer)
- Token persistence: `/tmp/<id>.tokens` (survives broker restarts)
- Poll cursor: `/tmp/room-<id>-<username>.cursor`
