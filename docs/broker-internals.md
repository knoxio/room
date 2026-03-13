# Broker internals

A technical reference for contributors and advanced users.

## Overview

The broker is a single async Tokio process that:

1. Listens on a Unix domain socket (`<runtime_dir>/room-<id>.sock`)
2. Accepts client connections and runs a per-client handler task
3. Fans out messages to all connected clients via a `tokio::broadcast` channel
4. Appends every event to an NDJSON chat file

> **Runtime directory:** `<runtime_dir>` is platform-native — macOS uses `$TMPDIR`
> (per-user, e.g. `/var/folders/...`), Linux uses `$XDG_RUNTIME_DIR/room/` or `/tmp/`
> as a fallback. See `paths::room_runtime_dir()`.

```
<runtime_dir>/room-<id>.sock  ←─────────────────────────┐
                                                         │
room myroom alice  ──[connect]──► Broker                 │
                                    │                    │
room myroom bob    ──[connect]──►   ├── handle_client(alice)
                                    │      ├── inbound task  (reads from socket)
room send ...      ──[connect]──►   │      └── outbound task (writes to socket)
                                    │
                                    ├── handle_client(bob)
                                    │      ├── inbound task
                                    │      └── outbound task
                                    │
                                    └── handle_oneshot_send (for room send / room join)
```

## Socket lifecycle

On startup the broker checks for a stale socket file at the expected path. If found, it removes it synchronously (using `std::fs::remove_file`, not `tokio::fs` — see [Why not `tokio::fs`](#why-not-tokiofs)) before binding. This handles the case where a previous broker crashed without cleaning up.

```rust
if self.socket_path.exists() {
    std::fs::remove_file(&self.socket_path)?;
}
let listener = UnixListener::bind(&self.socket_path)?;
```

The socket file is not removed on clean shutdown — it remains until the next broker start. Clients that connect after shutdown receive a connection error and must retry.

## Client connection protocol

Every connection begins with a single line of text that determines how the broker handles it:

| Prefix | Handled by | Description |
|---|---|---|
| `SEND:<username>` | `handle_oneshot_send` | Legacy one-shot send (no token) |
| `TOKEN:<uuid>` | `handle_oneshot_send` (after token lookup) | Token-authenticated one-shot send |
| `JOIN:<username>` | `handle_oneshot_join` | Token registration request |
| *(plain username)* | `handle_client` | Full interactive session |

### Interactive join handshake

For TUI and `--agent` connections, the first line is the bare username. The broker:

1. Registers the client in the `ClientMap` (username → broadcast sender)
2. Records the user as host if no host is set yet (first connected user)
3. Adds the user to the `StatusMap` with an empty status
4. Replays the full chat history, filtering DMs the user is not party to
5. Broadcasts a `join` event to all clients
6. Spawns inbound and outbound tasks (see below)

### Token handshake (`room join`)

A `JOIN:<username>` connection generates a UUID token and writes it to the broker's in-memory `TokenMap`. The token file on disk is written by the CLI (`room join`), not the broker.

If the username is already registered, the broker returns an error and closes the connection without issuing a token.

### One-shot send (`room send`)

A `TOKEN:<uuid>` connection resolves the UUID to a username via the `TokenMap`, then calls the same `handle_oneshot_send` path. The broker:

1. Reads one message line
2. Parses it (JSON command/message/dm or plain text)
3. Broadcasts and persists it
4. Echoes the broadcast record (with assigned `id`, `ts`, and `seq`) back to the sender
5. Closes the connection

No `join` or `leave` events are emitted for one-shot connections.

## Per-client tasks

For each interactive connection, the broker spawns two concurrent tasks:

### Inbound task

Reads lines from the client's socket. For each line:

- Parses it via `parse_client_line` into a `Message` enum variant
- Routes commands via `route_command`: `set_status`, `who`, `claim`, `unclaim`, `claimed`, and `room-info` are handled as built-in commands; admin commands go to `handle_admin_cmd`; plugin commands are dispatched via `PluginRegistry`; all others pass through to broadcast
- DMs (`Message::DirectMessage`) are delivered only to sender, recipient, and host via `dm_and_persist`
- Everything else goes to `broadcast_and_persist`

The inbound task exits when the client disconnects (EOF) or on a read error.

### Outbound task

Receives from the broadcast channel and forwards lines to the client socket. Also listens for the shutdown signal:

```rust
loop {
    tokio::select! {
        result = rx.recv() => {
            // forward message to socket
        }
        _ = shutdown_rx.changed() => {
            // drain pending messages, call write_half.shutdown(), break
        }
    }
}
```

The outbound task exits on broadcast channel close, write error, or shutdown signal.

`handle_client` waits for whichever task exits first (via `tokio::select!` on both `JoinHandle`s), then broadcasts a `leave` event and removes the user from the `StatusMap`.

## Message fan-out

Every broadcast goes through a single `tokio::broadcast::channel::<String>(256)`. Each interactive client holds a receiver. When `broadcast_and_persist` is called:

1. Assigns the next sequence number (monotonic `AtomicU64`)
2. Appends the message to the NDJSON chat file
3. Serialises the message to a JSON line
4. Sends the line to the broadcast channel — all receivers get a copy

If a receiver lags by more than 256 messages, it receives `RecvError::Lagged` and the lagged count is logged. Messages are not re-delivered.

### Sequence numbers

Every persisted message gets a monotonically increasing `seq` field, assigned by the broker via an `AtomicU64` counter. Clients and tooling can use `seq` to detect gaps in delivery.

## Shutdown signal

The broker uses `tokio::sync::watch::channel::<bool>(false)` as its shutdown signal. The sender (`Arc<watch::Sender<bool>>`) is stored in `RoomState`. Receivers are created with `sender.subscribe()` before each outbound task is spawned.

When `/exit` is processed by `handle_admin_cmd`:

1. Broadcasts a system shutdown notice to all clients
2. Calls `shutdown.send(true)` on the watch sender

Every outbound task's `shutdown_rx.changed()` arm fires on the next `select!` iteration. Because watch stores state (the current value is `true`), tasks that reach the select after the send still see the change immediately — there is no race window.

The main accept loop also holds a watch receiver and exits when `changed()` fires.

> **Why not `Arc<Notify>`?** `Notify::notify_waiters()` only wakes futures that are currently registered (polled). If the outbound task is mid-write when the signal fires, its `notified()` future is not yet registered, and the wakeup is silently dropped. `watch` avoids this by storing the signal value.

## NDJSON persistence

The chat file (default: `~/.room/data/<room-id>.chat`) is an NDJSON file — one JSON object per line. The broker is the **sole writer**; clients must never write to it directly.

All writes go through `history::append`, which opens the file in append mode. Tokio's `tokio::fs` wrappers are not used here — see below.

`room poll` and `room watch` read the chat file directly without a socket connection. Multiple concurrent readers are safe because append-only writes are atomic for small payloads on local filesystems.

## Why not `tokio::fs`

`tokio::fs` operations are dispatched to a blocking thread pool managed by Tokio. If the runtime is shutting down, those threads may have already stopped, causing `tokio::fs` calls to be silently cancelled or to hang. All file I/O in `room` uses `std::fs` (synchronous) or explicit `spawn_blocking` wrappers to avoid this.

## Shared state

`RoomState` is wrapped in `Arc` and passed to every client handler:

| Field | Type | Description |
|---|---|---|
| `clients` | `Arc<Mutex<HashMap<u64, (String, broadcast::Sender<String>)>>>` | Maps client ID to username + broadcast sender |
| `status_map` | `Arc<Mutex<HashMap<String, String>>>` | Maps username to status string (ephemeral) |
| `host_user` | `Arc<Mutex<Option<String>>>` | Username of the first connected client |
| `token_map` | `Arc<Mutex<HashMap<String, String>>>` | Maps token UUID to username (persisted to `.tokens` file) |
| `claim_map` | `Arc<Mutex<HashMap<String, ClaimEntry>>>` | Maps username to claim entry (task + timestamp, 30min TTL, lazily swept) |
| `chat_path` | `Arc<PathBuf>` | Path to the NDJSON chat file |
| `room_id` | `Arc<String>` | Room identifier |
| `shutdown` | `Arc<watch::Sender<bool>>` | Shutdown signal |
| `seq_counter` | `Arc<AtomicU64>` | Monotonic sequence counter |
| `plugin_registry` | `Arc<PluginRegistry>` | Compiled-in plugin dispatch (`/help`, `/stats`, `/queue`, `/taskboard`) |
| `config` | `Option<RoomConfig>` | Room visibility and access control (daemon mode) |

## Admin commands

Admin commands are restricted to the room host (first connected user). They are routed through `handle_admin_cmd` when received as a `Message::Command` with a `cmd` matching `ADMIN_CMD_NAMES`:

```rust
const ADMIN_CMD_NAMES: &[&str] = &["kick", "reauth", "clear-tokens", "exit", "clear"];
```

| Command | Effect |
|---|---|
| `kick <user>` | Removes all tokens for `<user>`, inserts a `KICKED:<user>` sentinel so the username stays reserved. Removes from `StatusMap`. |
| `reauth <user>` | Removes all tokens for `<user>` (including the kicked sentinel) and deletes the on-disk token file. The user can `room join` again. |
| `clear-tokens` | Clears the full `TokenMap` and removes all token files from `~/.room/state/`. |
| `exit` | Broadcasts a shutdown notice, then calls `shutdown.send(true)`. |
| `clear` | Truncates the chat history file via `std::fs::write(path, "")`. |
