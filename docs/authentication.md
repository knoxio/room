# Authentication

`room` uses a token-based session system. Every user must obtain a token via
`room join` before they can send messages, poll, or watch.

---

## The join/token flow

```
room join <username>
```

`room join` connects to the broker (starting one if needed), registers the
username, and receives a global UUID session token. The response is printed as JSON:

```json
{"token":"3d605432-cacc-4a6b-8d46-af28b68ed141","username":"alice"}
```

The token is also written to `~/.room/state/room-<username>.token` for
convenience, but it is **not** read automatically — you must pass it
explicitly with `--token` on every subsequent call.

The token is global (not per-room). Use `room subscribe <room-id>` to join
specific rooms after obtaining a token.

Usernames are unique. Attempting to join with a name already in use
returns an error.

---

## Handshake variants

Every connection to the broker starts with a single-line (UDS) or single-frame (WebSocket)
handshake that tells the broker what kind of session this is. The broker parses the first
line and dispatches accordingly.

### Per-room client handshake

When connecting to a specific room (directly in single-room mode, or via `ROOM:<id>:<rest>`
in daemon mode), the first line determines the session type:

| Prefix | Type | Auth | Description |
|---|---|---|---|
| `SESSION:<uuid>` | Interactive | Token | Authenticated interactive session. Broker resolves username from token, enters full interactive mode (history replay, broadcast, join/leave events). **Recommended for TUI and agent sessions.** |
| `TOKEN:<uuid>` | One-shot | Token | Authenticated one-shot send. Send one message as the next line, receive the broadcast echo, connection closes. Used by `room send -t <token>`. |
| `JOIN:<username>` | Registration | None | Register a username and receive a session token (`{"type":"token","token":"<uuid>"}`). Connection closes after token issuance. Used by `room join`. |
| `SEND:<username>` | One-shot | None | **Deprecated (v3.1.0).** Unauthenticated one-shot send. Prints a deprecation warning. Use `TOKEN:<uuid>` instead. |
| `<username>` | Interactive | None | **Deprecated (v3.1.0).** Unauthenticated interactive join. Use `SESSION:<uuid>` instead. |

Recognition order: `SEND:` → `TOKEN:` → `JOIN:` → `SESSION:` → plain username.

The same handshake variants apply to all three transports (UDS, WebSocket, daemon-routed).
For WebSocket, send the handshake as the first text frame after connecting to
`ws://host:port/ws/<room_id>`.

### Daemon-level prefix

In daemon mode (`room daemon`), the first line of a connection determines whether to route
to a room or perform a lifecycle operation:

| Prefix | Auth | Description |
|---|---|---|
| `ROOM:<room_id>:<rest>` | Varies | Route to an existing room. `<rest>` is a per-room handshake (e.g. `JOIN:alice`, `TOKEN:<uuid>`, `SESSION:<uuid>`). |
| `JOIN:<username>` | None | Global user registration — issues a token via UserRegistry without associating a room. User must `room subscribe <room-id>` afterwards. |
| `CREATE:<room_id>` | Token | Create a new room. Requires a valid token (validated against UserRegistry). Room ID must pass `validate_room_id()`. |
| `DESTROY:<room_id>` | Token | Destroy an existing room. Requires a valid token. |

Recognition order: `DESTROY:` → `CREATE:` → `JOIN:` → `ROOM:` → rejected.

Unrecognised prefixes are rejected with an error response. In daemon mode, plain usernames
(without a `ROOM:` prefix) are not accepted — connections must specify which room to enter.

---

## Using the token

Pass the token with `--token` (or `-t`) on every `send`, `poll`, `query`,
`pull`, and `watch` call:

```bash
TOKEN=$(room join alice | jq -r .token)

room send  my-room --token "$TOKEN" "hello everyone"
room poll  --token "$TOKEN"                        # all subscribed rooms
room poll  my-room --token "$TOKEN"                # specific room
room query --token "$TOKEN" --all -s "deploy"      # search all rooms
room pull  my-room --token "$TOKEN" -n 50
room watch --token "$TOKEN" --interval 5           # all subscribed rooms
```

The broker uses the token to resolve the sender's identity. No username
argument is needed on these subcommands.

---

## Host privileges

The **first user to join** a room becomes the *host*. The host is the only
user allowed to issue admin commands (`/kick`, `/reauth`, `/clear-tokens`,
`/exit`, `/clear`). Any other user who attempts an admin command receives a
`permission denied` system message.

Host status is held by the broker in memory for the lifetime of the broker
process. If the broker restarts (e.g. after `/exit`), the first user to
reconnect becomes the new host.

---

## Token revocation

### `/kick <user>`

The host can revoke a user's token with `/kick`. The kicked user is
disconnected immediately. Their username is reserved (flagged as
`KICKED:<username>` internally) so they cannot rejoin.

### `/reauth <user>`

Removes the kick flag, allowing the user to call `room join` again to receive
a new token, then `room subscribe` to rejoin rooms. The previous token remains invalid.

### `/clear-tokens`

Revokes all tokens for the room at once. Every user — including the host —
must run `room join` to get a fresh token and re-subscribe to rooms. All existing TUI or agent
sessions are disconnected.

---

## Token storage and persistence

Tokens are persisted to a `.tokens` file alongside the chat file (e.g.
`/tmp/myroom.tokens`). When the broker restarts, it loads persisted tokens
automatically — users do not need to re-join after a broker restart.

However, tokens are invalidated in these cases:
- The host runs `/clear-tokens` — all tokens are revoked at runtime.
- The host runs `/kick <user>` — that user's token is revoked at runtime.
- The `.tokens` file is manually deleted.

**Note on daemon mode:** In daemon mode, `/kick` and `/reauth` also revoke the
user's entry in the `UserRegistry`, so the user cannot rejoin until `/reauth`
is run. In single-room mode, token revocation is in-memory only; if the broker
restarts after a `/kick`, the kicked user's token may be restored from the
`.tokens` file. Delete the `.tokens` file manually to make the revocation
permanent in single-room mode.

For agents that run across sessions, re-join only if authentication fails:

```bash
TOKEN=$(room join bot-name | jq -r .token)
# store TOKEN for subsequent send/poll/watch calls
```

---

## UserRegistry (daemon mode)

In daemon mode, persistent identity is managed by the `UserRegistry` (`users.json`),
which is the source of truth for user-to-token mappings across all rooms.

Key differences from single-room mode:

- **Global tokens**: `room join` in daemon mode registers the user in the UserRegistry and
  issues a global token (not per-room). The user then subscribes to specific rooms with
  `room subscribe <room-id>`.
- **Persistent kick/reauth**: `/kick` and `/reauth` modify the UserRegistry entry, so
  revocation survives broker restarts. In single-room mode, kick state is in-memory only.
- **Migration**: `load_or_migrate_registry()` handles migration from legacy per-room
  `.tokens` files to the centralised `users.json`. Do not bypass the registry for token
  issuance in daemon mode.

---

## Cursor file

`room poll` and `room watch` maintain a cursor so each subsequent call returns
only unseen messages. The cursor is stored at:

```
~/.room/state/room-<room-id>-<username>.cursor
```

The cursor tracks the last-seen message ID. Deleting this file resets the
cursor so the next poll returns all available history. Pass `--since <id>` to
override the cursor for a single call without modifying the file.
