# Changelog

All notable changes to `room-cli` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

For changes prior to the workspace restructure (v0.1.2 through v1.0.2), see the
[root CHANGELOG](../../CHANGELOG.md).

## [Unreleased]

### Fixed

- Oneshot commands returning `CommandResult::Handled` (e.g. `/taskboard post`) now send a
  system ack response instead of closing the socket with no reply, which caused EOF parse
  errors and broker crashes. Affects UDS and WS oneshot transports.
- UTF-8 panic in `/taskboard list` — byte-level string truncation replaced with char-level
  truncation to prevent panics on multibyte characters at the slice boundary.

## [3.1.0] - 2026-03-13

### Added

- Event filtering on subscriptions: `room subscribe <room> -t TOKEN full --events task_posted,task_finished`
  filters which event types appear in poll results. Supports `all` (default), `none`,
  or comma-separated list. Persisted per-user per-room as `.event_filters` files. (#430)
- `/subscribe_events` broker command — set event type filter independently of subscription tier. (#430)
- `/subscriptions` now shows event filters alongside tier subscriptions. (#430)
- Taskboard plugin emits typed `Event` messages (from room-protocol) alongside
  system broadcasts — first consumer of the event type system. (#430)
- TUI: render `Event` messages with `[event:<type>]` tag in yellow. (#430)
- `/taskboard cancel <task-id> [reason]` subcommand — cancels a task with optional reason.
  Permission: poster, assignee, or host. Finished/cancelled tasks cannot be re-cancelled. (#506)
- Integration tests: Event variant flows through UDS, WebSocket, and REST
  transports — lifecycle events, broadcast, persistence, wire format. (#430)
- Integration tests: REST/WS global daemon token fallback and kicked user WS reconnection rejection. (#490, #492)
- Integration tests: queue plugin oneshot response — add, pop (FIFO), and remove return system echo to oneshot senders. (#494)
- `/taskboard assign <task-id> <username>` subcommand — poster or host can assign an open task to a specific user. (#502)
- Integration tests: taskboard oneshot response — post, list, and claim+finish lifecycle return system echo to oneshot senders. (#534)
- `/info [username]` command — shows room metadata (no args) or user info (status,
  subscription tier, host flag, online status). `/room-info` is now an alias. (#507)

### Fixed

- TUI: ellipsize long status text in member panel with `…` instead of
  silently clipping at the border. (#497)
- Admin `/kick` and `/reauth` now strip leading `@` from target username, fixing token invalidation failure and kicked users not being removed from the TUI member panel. (#505)
- TUI markdown renderer: support `***bold+italic***`, nested `@mentions` and
  `*italic*` inside `**bold**`, triple-backtick code block fencing, and
  backtick edge cases. (#498)

### Changed

- `/set_status` moved from plugin to builtin command — no behavior change, removes
  `PluginResult::SetStatus` variant and `plugin/status.rs`. (#508)
- `/help` moved from plugin to builtin command — handled directly in `route_command` alongside `/who` and `/room-info`, eliminating the circular dependency on `CommandContext.available_commands`. (#509)

### Removed

- `/claim`, `/unclaim`, `/claimed` builtins and backing `ClaimMap`/`ClaimEntry` types.
  Queue `/pop` no longer auto-claims; it broadcasts "popped from queue" instead. (#510)

### Fixed

- Standalone broker now registers all plugins (queue, taskboard) — previously only help, stats, status were available outside daemon mode. (#513)

## [3.0.1] - 2026-03-12

### Added

- `/taskboard` plugin — lease-based task lifecycle with `post`, `list`, `show`,
  `claim`, `plan`, `approve`, `update`, `release`, `finish` subcommands. Tasks have
  lease TTLs with auto-expiry. (#444, #451, #453)
- `/queue` plugin — persistent FIFO queue with NDJSON storage. Subcommands: `add`,
  `list`, `remove`, `pop`. (#442)
- TUI: `ChoicePicker` widget for command parameter autocomplete. (#437, #445)
- TUI: vertically center splash screen in the message area. (#427, #428)
- Lease TTL on `/claim` with automatic expiry of stale claims. (#429, #443)

### Fixed

- Broker echoes plugin broadcast messages back to the oneshot sender. (#450)
- Oneshot `watch` filter now includes system messages from plugins. (#452)
- Persist subscription state on join so it survives broker restarts. (#438)
- TUI: arrow-down on last line moves cursor to end before scrolling. (#422, #424)

## [3.0.0-rc.6] - 2026-03-10

### Fixed

- Admin `/kick` and `/reauth` now revoke `UserRegistry` tokens in daemon mode,
  unblocking the user for `room join` after a kick/reauth. (#402)
- `discover_joined_rooms` now checks subscription state instead of stale
  per-room token files. (#412, #413)
- Global join tokens (from `room join <username>`) now validate correctly for
  room-level `TOKEN:` sends. (#414, #415)
- WS/REST `Bearer` token validation falls back to `UserRegistry` for global
  tokens. (#416, #418)

### Added

- `room daemon --isolated` flag: spawns a daemon with a private temp dir for
  tests, prints socket path to stdout, and cleans up on exit. (#398, #417)

## [3.0.0-rc.5] - 2026-03-10

### Changed

- `room join` no longer takes a room-id argument. It now issues a **global**
  token (`room join <username>`) tied to the user, not to a specific room.
  Use `room subscribe <room-id>` to subscribe to rooms after joining. (#388)

### Fixed

- Per-room token file cleanup: removed stale `token_file_path` and related
  functions from the oneshot code path. (#411)

### Added

- TUI: inline markdown rendering — `**bold**`, `*italic*`, `` `code` ``, and
  `- list items` rendered with terminal attributes. (#405, #410)
- TUI: subscription tier indicators in the member status panel. (#399, #404)

## [3.0.0-rc.3] - 2026-03-09

### Added

- TUI: use daemon exclusively for room join — all `room <room-id> <user>` sessions
  now go through the daemon. (#385, #391)
- `/claim`, `/unclaim`, `/claimed` built-in commands for task coordination. (#182)
- Parameter validation for built-in commands against `CommandInfo` schemas. (#265)
- Token persistence across broker restarts via `.tokens` files. (#183)
- Room ID sanitization — rejects invalid characters. (#264)
- Multiplexed poll: `room poll --rooms r1,r2,r3` polls multiple rooms. (#255)
- `--mentions-only` filter on `room poll`. (#256)
- Reusable test harness in `tests/common/mod.rs`. (#181)
- Pre-scripted multi-agent test scenarios in integration tests. (#180)

### Fixed

- `room join` now sets `Full` subscription to prevent auto-subscribe downgrade. (#392)
- TUI: redirect stderr to `~/.room/room.log` during TUI sessions. (#395)
- DM host visibility — `is_visible_to()` now checks host membership for DM
  rooms so the host can see all DMs in their room. (#394, #407)
- WS smoke tests clean up stale `.tokens` files from previous runs. (#184)
- Serialized WS smoke test execution to prevent disk I/O contention. (#184)

## [3.0.0-rc.2] - 2026-03-09

### Added

- Sprint 9 refactor wave: `handshake.rs`, `admin.rs`, `render_bots.rs`, `ws/rest.rs`
  split, `paths.rs`, Plugin trait defaults, RoomState factory and accessors, handle\_key
  decomposition, `is_visible_to()` on Message. (#351–#379)
- P0 test coverage: duplicate-username join, concurrent room creation, destroy with
  connected clients, host disconnect/reconnect + DM history replay. (#381–#384)

### Fixed

- Daemon auto-start timeout increased to 15s to handle slow volumes. (#380)

## [3.0.0-rc.1] - 2026-03-08

### Added

- Multi-room daemon (`room daemon`) with shared UDS socket and optional
  WebSocket. (#251, #261)
- Room visibility and ACLs — public, private, unlisted, and DM types with
  invite lists and `is_visible_to()` checks. (#253, #263)
- WebSocket + REST transport (`--ws-port`): `JOIN`, `TOKEN`, `SEND` handshakes,
  full REST API (`/api/<room>/join`, `/send`, `/poll`, `/query`). (#254)
- `room create` / `room destroy` subcommands for daemon room lifecycle. (#264)
- `room list` subcommand — lists known rooms and their metadata. (#264)
- `--socket` flag on `join`, `send`, `who`, `poll`, `watch`, `dm`. (#308)
- `RoomService` trait for dependency-injected REST handlers. (#363)
- Comprehensive integration test split: auth, broker, daemon, oneshot,
  rest\_query, room\_lifecycle, scripted, ws. (#355)
- `clippy.toml` enforcing cognitive-complexity ≤ 30, too-many-lines ≤ 600,
  too-many-arguments ≤ 7. (#364)

## [2.1.0-rc.2] - 2026-03-08

### Fixed

- TUI: kicked users are now removed from the member status panel and @-mention autocomplete when the broker broadcasts a kick message. (#236)

## [2.1.0-rc.1] - 2026-03-07

### Added

- `room who <room-id> -t <token>` oneshot subcommand to query online members and statuses. (#226)
- TUI: floating member status panel (top-right) showing connected users and their `/set_status` text. Auto-hides on narrow terminals. (#225)
- Oneshot slash command routing: `room send` now correctly routes `/who`, `/set_status`, `/dm` as command envelopes instead of plain messages. (#223)

## [2.0.1] - 2026-03-06

### Added

- TUI: version displayed in window border title. (#210)
- TUI: tagline and build date shown under splash logo. (#210)
- Crate-level README for crates.io. (#217)

## [2.0.0] - 2026-03-06

### Changed

- Renamed package from `agentroom` to `room-cli`. Binary remains `room`. (#198, #205)
- Extracted wire format types into `room-protocol` crate. `room-cli` depends on
  `room-protocol` for message types. (#197, #204)
- Moved source from `src/` to `crates/room-cli/src/` as part of workspace
  restructure.

### Added

- All existing functionality from `agentroom` 1.0.2: broker, TUI, one-shot
  subcommands (`join`, `send`, `poll`, `watch`, `list`), WebSocket/REST transport,
  plugin system (`/help`, `/stats`), admin commands, DMs, agent mode.
- 312 tests (236 unit + 71 integration + 5 smoke).

[Unreleased]: https://github.com/knoxio/room/compare/v3.0.0-rc.6...HEAD
[3.0.0-rc.6]: https://github.com/knoxio/room/compare/v3.0.0-rc.5...v3.0.0-rc.6
[3.0.0-rc.5]: https://github.com/knoxio/room/compare/v3.0.0-rc.3...v3.0.0-rc.5
[3.0.0-rc.3]: https://github.com/knoxio/room/compare/v3.0.0-rc.2...v3.0.0-rc.3
[3.0.0-rc.2]: https://github.com/knoxio/room/compare/v3.0.0-rc.1...v3.0.0-rc.2
[3.0.0-rc.1]: https://github.com/knoxio/room/compare/v2.1.0-rc.2...v3.0.0-rc.1
[2.1.0-rc.2]: https://github.com/knoxio/room/compare/v2.1.0-rc.1...v2.1.0-rc.2
[2.1.0-rc.1]: https://github.com/knoxio/room/compare/v2.0.1...v2.1.0-rc.1
[2.0.1]: https://github.com/knoxio/room/compare/v2.0.0...v2.0.1
[2.0.0]: https://github.com/knoxio/room/releases/tag/v2.0.0
