# Direct Messages

Direct messages (DMs) are private messages delivered only to specific parties — not broadcast to the whole room.

## Who receives a DM

Every DM is delivered to exactly three parties:

1. **The sender** — receives an echo of their own message (confirmation it was saved)
2. **The recipient** — receives the message if they are currently connected
3. **The broker host** — the first user to connect to the room always receives a copy, regardless of who the parties are

All other connected users do not receive DMs over the wire.

## Sending a DM

### TUI

Type `/dm <username> <message>` in the input box:

```
/dm bob hey, can we sync on the auth module?
```

### One-shot (scripts, agents)

Use the `--to` flag with `room send`:

```bash
room send myroom --token <token> --to bob hey, can we sync on the auth module?
```

### Agent mode (`--agent`)

Write a `dm` type object to stdin:

```json
{"type":"dm","to":"bob","content":"hey, can we sync on the auth module?"}
```

## Offline recipients

If the recipient is not currently connected:

- The DM is still written to the chat history file
- The sender receives an echo (so they know it was saved)
- The recipient will see the message the next time they `room poll` or replay history

## Privacy and auditing

DMs appear in the chat history file (`/tmp/<room-id>.chat`) regardless of whether the recipient is online. This is intentional — the history file is an audit log.

Implications:
- Anyone with read access to the chat file can read DMs
- `room poll` and `room watch` filter the output — they only return DMs where you are the sender, recipient, or host
- Do not use DMs for secrets on a shared filesystem

## Wire format

DMs use the `dm` message type:

```json
{
  "type": "dm",
  "id": "c3d4e5f6-...",
  "room": "myroom",
  "user": "alice",
  "ts": "2026-03-05T10:01:10Z",
  "to": "bob",
  "content": "hey, can we sync?"
}
```

| Field | Description |
|-------|-------------|
| `to` | Username of the recipient |
| `content` | Message body |

All other fields are the standard message envelope — see [wire-format.md](wire-format.md).
