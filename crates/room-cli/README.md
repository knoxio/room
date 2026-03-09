# room-cli

Multi-user chat room for agent and human coordination over Unix domain sockets and WebSocket.

`room-cli` provides the `room` binary — a broker that manages chat rooms, a full-screen TUI for interactive use, and one-shot subcommands for scripting and AI agent integration.

## Installation

```bash
cargo install room-cli
```

The installed binary is named `room`.

## Quick start

```bash
# Terminal 1 — starts the broker and opens the TUI
room myroom alice

# Terminal 2 — joins the existing room
room myroom bob
```

The first invocation in a given room starts the broker automatically. Subsequent invocations connect as clients.

## Subcommands

| Command | Description |
|---------|-------------|
| `room <room-id> <username>` | Start or join a room (TUI mode) |
| `room join <username>` | Register and get a global session token |
| `room send <room-id> -t <token> <message>` | Send one message and exit |
| `room query [-t <token>] [OPTIONS]` | Query messages with filters, search, subscriptions |
| `room poll [<room-id>] -t <token>` | Alias for `query --new` — print new messages and exit |
| `room pull <room-id> -t <token> [-n N]` | Fetch last N messages without updating cursor |
| `room watch [<room-id>] -t <token>` | Alias for `query --new --wait` — block until a message arrives |
| `room who <room-id> -t <token>` | Query online members and their statuses |
| `room list` | List active rooms with running brokers |

## TUI features

- Full-screen terminal interface built with [ratatui](https://github.com/ratatui-org/ratatui)
- Version displayed in border, splash screen with tagline
- Slash commands: `/who`, `/dm <user> <msg>`, `/set_status <msg>`, `/claim <task>`
- Admin commands: `/kick`, `/reauth`, `/clear-tokens`, `/exit`, `/clear`
- Floating member status panel (top-right) showing online users and their `/set_status` text
- Command palette with tab completion and `@` mention picker
- Multi-line input with `Shift+Enter`
- Message history scrolling with arrow keys / PageUp / PageDown

## Agent integration

For AI agents that use sequential tool calls (e.g. Claude Code):

```bash
# 1. Register once per broker session (global token)
room join agent1
# → {"type":"token","token":"<uuid>","username":"agent1"}

# 2. Send messages
room send myroom -t <token> "starting work on #42"

# 3. Poll for new messages (all subscribed rooms)
room poll -t <token>

# 4. Watch all subscribed rooms (block until a message arrives)
room watch -t <token> --interval 5

# 5. Query with filters
room query -t <token> --all -s "bug" --user alice -n 10

# 6. Query who is online
room who myroom -t <token>
# → online — agent1, alice: reviewing PR
```

See the [agent coordination protocol](../../CLAUDE.md) for the full multi-agent workflow.

## WebSocket / REST transport

Start the broker with `--ws-port` to enable HTTP access alongside Unix sockets:

```bash
room myroom alice --ws-port 4200
```

Endpoints:

| Method | Endpoint | Description |
|--------|----------|-------------|
| `POST` | `/api/<room>/join` | Register username, get token |
| `POST` | `/api/<room>/send` | Send a message (Bearer auth) |
| `GET` | `/api/<room>/poll` | Poll for messages (Bearer auth) |
| `GET` | `/api/health` | Health check |
| `ws://` | `/ws/<room>` | WebSocket connection |

## Documentation

- [Full CLI reference and wire format](../../README.md)
- [Agent coordination protocol](../../CLAUDE.md)
- [Ralph setup guide](../../docs/ralph-setup.md) — autonomous agent wrapper

## License

MIT
