# Deployment and configuration

For users running `room` in production or custom environments.

## Default paths

| Resource | Default path | Controlled by |
|---|---|---|
| Broker socket | `/tmp/room-<id>.sock` | Fixed (derived from room ID) |
| Chat history | `/tmp/<id>.chat` | `-f <path>` flag when starting a new room |
| Session tokens | `/tmp/room-<id>-<username>.token` | Fixed |
| Poll cursor | `/tmp/room-<id>-<username>.cursor` | Fixed |

## Changing the chat file path

Pass `-f <path>` to the first invocation that starts the broker:

```bash
room myroom alice -f /var/log/myroom.chat
```

Clients that join later connect to the existing broker and use the path it was started with. The `-f` flag is ignored for connections that join an already-running broker.

## File permissions

The broker and all clients must share access to the socket and chat file:

- **Socket** (`/tmp/room-<id>.sock`): all users connecting to the room need read+write permission. The socket is created by the broker process with the broker owner's umask.
- **Chat file**: the broker must have write access; `room poll` and `room watch` clients need read access.

For shared machines, run the broker as a shared user, or ensure the socket and chat file are group-writable for the relevant group.

## Running multiple rooms on one machine

Each room has a separate socket and chat file derived from its ID. Rooms are fully isolated — different room IDs never share state:

```bash
# Terminal 1: room "alpha"
room alpha alice -f /tmp/alpha.chat

# Terminal 2: room "beta"
room beta bob -f /tmp/beta.chat
```

Choose room IDs that are unlikely to collide across users on the same machine.

## Broker lifecycle

### Auto-start

The broker starts automatically when the first `room <id> <username>` invocation finds no socket at `/tmp/room-<id>.sock`. Subsequent invocations connect as clients.

### Stale socket cleanup

If the broker crashed or was killed without removing its socket, the next startup detects the stale file and removes it before binding. This is handled synchronously at startup — no manual cleanup is needed.

### Clean shutdown

The room host can shut down the broker with `/exit` from the TUI (or via `room send` with the JSON command envelope). This:

1. Broadcasts a shutdown notice to all connected clients
2. Sends EOF to all client sockets (outbound tasks drain pending messages first)
3. Exits the broker process

Connected TUI clients detect the EOF and exit cleanly — no `Ctrl-C` required.

### Crash recovery

If the broker exits unexpectedly, connected clients receive EOF and their TUI sessions exit. To restart:

```bash
room myroom alice -f /path/to/existing.chat
```

The existing chat file is replayed to reconnecting clients. Tokens issued before the crash are gone (in-memory only); users must `room join` again to get new tokens.

## Persisting history across restarts

The chat file survives broker restarts as long as it is not deleted. Point all broker invocations at the same file:

```bash
room myroom alice -f /persistent/myroom.chat
```

History is replayed to every client on join. Use `-n <N>` to control how many recent messages are shown (default: 20); the full file is always replayed for DM filtering purposes.

To clear history while the broker is running, use `/clear` from a host TUI session — this truncates the file and broadcasts a notice.

## Running as a service

> **Note:** the following examples are provided as a starting point and have not been tested in production. Adjust paths, users, and socket permissions for your environment.

### systemd (Linux)

Create `/etc/systemd/system/room-myroom.service`:

```ini
[Unit]
Description=room broker — myroom
After=network.target

[Service]
Type=simple
User=myuser
ExecStart=/usr/local/bin/room myroom broker -f /var/lib/room/myroom.chat
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Then:

```bash
sudo systemctl enable --now room-myroom
```

The `broker` username is a placeholder — replace with the actual first user or a dedicated service account. The socket path is still `/tmp/room-myroom.sock` (or use a custom path if you modify the source).

### launchd (macOS)

Create `~/Library/LaunchAgents/com.example.room-myroom.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.example.room-myroom</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/myuser/.cargo/bin/room</string>
    <string>myroom</string>
    <string>broker</string>
    <string>-f</string>
    <string>/Users/myuser/.local/share/room/myroom.chat</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
```

Then:

```bash
launchctl load ~/Library/LaunchAgents/com.example.room-myroom.plist
```
