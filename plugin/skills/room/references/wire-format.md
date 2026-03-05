# Room wire format reference

Every message in `room poll` output and in the chat file is a JSON object with a `type` field.

## Common fields (all types)

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Message type (see below) |
| `id` | string | UUID v4 assigned by the broker |
| `room` | string | Room identifier |
| `user` | string | Username of sender |
| `ts` | string | ISO 8601 timestamp (UTC) |

## Types

### `message` — plain chat message
```json
{"type":"message","id":"...","room":"myproject","user":"alice","ts":"...","content":"hello"}
```

### `join` / `leave` — connection events
```json
{"type":"join","id":"...","room":"myproject","user":"alice","ts":"..."}
{"type":"leave","id":"...","room":"myproject","user":"alice","ts":"..."}
```

### `command` — structured command
```json
{"type":"command","id":"...","room":"myproject","user":"alice","ts":"...","cmd":"claim","params":["src/auth.rs"]}
```

### `reply` — threaded reply
```json
{"type":"reply","id":"...","room":"myproject","user":"bob","ts":"...","reply_to":"<message-id>","content":"ack"}
```

### `system` — broker-generated notice
```json
{"type":"system","id":"...","room":"myproject","user":"broker","ts":"...","content":"alice set status: working on auth"}
```

### `dm` — private direct message
Delivered only to sender, recipient, and the broker host. Still persisted to history.
```json
{"type":"dm","id":"...","room":"myproject","user":"alice","ts":"...","to":"bob","content":"hey, can we sync?"}
```

## Sending via `--agent` stdin (persistent mode only)

When using `room <id> <username> --agent`, write to stdin:

```json
{"type":"message","content":"your message"}
{"type":"reply","reply_to":"<id>","content":"ack"}
{"type":"command","cmd":"claim","params":["src/auth.rs"]}
{"type":"dm","to":"bob","content":"private message"}
```

The broker assigns `id`, `room`, `user`, and `ts` — do not send them.
Plain text is also accepted and treated as a `message`.
