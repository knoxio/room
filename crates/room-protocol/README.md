# room-protocol

Wire format types for the [room](https://github.com/knoxio/room) multi-user chat system.

This crate defines the `Message` enum and constructor functions shared by all room components (broker, TUI, agent wrappers, future UIs). It contains no transport or IO logic — just types and serialization.

## Installation

```toml
[dependencies]
room-protocol = "3"
```

## Message types

All messages use a flat, internally-tagged JSON format (`#[serde(tag = "type")]`):

| Type | Description | Key fields |
|------|-------------|------------|
| `join` | User joined the room | `user` |
| `leave` | User left the room | `user` |
| `message` | Chat message | `user`, `content` |
| `reply` | Reply to a specific message | `user`, `reply_to`, `content` |
| `command` | Slash command | `user`, `cmd`, `params` |
| `system` | Broker system message | `user`, `content` |
| `direct_message` | Private message | `user`, `to`, `content` |

Every variant carries `id` (UUID), `room`, `user`, `ts` (UTC timestamp), and an optional `seq` (sequence number).

## Usage

```rust
use room_protocol::{Message, make_message, parse_client_line};

// Create a message
let msg = make_message("myroom", "alice", "hello world");

// Serialize to JSON (NDJSON wire format)
let json = serde_json::to_string(&msg).unwrap();

// Deserialize from JSON
let parsed: Message = serde_json::from_str(&json).unwrap();

// Parse raw client input (plain text or JSON envelope)
let msg = parse_client_line("hello", "myroom", "bob").unwrap();
```

## Public API

- `Message` — enum with variants for all wire format types
  - `content()` — returns the content/text of any message variant
  - `mentions()` — extracts `@username` mentions from message content
- `make_join`, `make_leave`, `make_message`, `make_reply`, `make_command`, `make_system`, `make_dm` — constructors
- `parse_client_line` — parse raw client input (plain text becomes a `Message`, JSON envelopes are deserialized)
- `parse_mentions` — extract `@username` mentions from arbitrary text
- `RoomConfig`, `RoomVisibility` — room access control types
- `dm_room_id` — deterministic room ID for DM pairs

## Design constraints

- **No `#[serde(flatten)]`** with `#[serde(tag)]` — this combination breaks deserialization in serde. Each variant carries its own fields.
- **No transport logic** — this crate is types-only. Consumers handle their own IO.
- **Stable wire format** — changes to the `Message` enum are breaking changes that require coordination.

## License

MIT
