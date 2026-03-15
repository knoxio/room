# Changelog

All notable changes to `room-ralph` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `--heartbeat-interval <N>` flag and `RALPH_HEARTBEAT_INTERVAL` environment variable
  for periodic `/set_status` heartbeat updates every N iterations (default: 5).
  Includes iteration count and wall-clock uptime. Set to 0 to disable. (#750)

## [3.4.0] - 2026-03-15

## [3.3.0] - 2026-03-15

## [3.2.0] - 2026-03-13

## [3.1.0] - 2026-03-13

### Added

- `--allow-all` flag to skip all tool restrictions for trusted environments. (#433, #440)
- Builtin personality templates — predefined personality files (`Coder`, `Reviewer`,
  `Coordinator`, etc.) selectable via `--personality builtin:<name>` without an
  external file. (#439, #446)

## [0.4.0-rc.6] - 2026-03-10

Version bump to stay in sync with room-cli 3.0.0-rc.6. No functional changes.

## [0.4.0-rc.5] - 2026-03-10

### Changed

- Updated for global token migration: `room join` no longer requires a room-id
  argument. Ralph now calls `room join <username>` and `room subscribe <room-id>`
  separately. (#408)

### Fixed

- Retry with suffixed username (e.g. `alice-1`) when `room join` fails due to
  duplicate username. (#403)

## [0.4.0-rc.3] - 2026-03-09

Version bump to stay in sync with room-cli 3.0.0-rc.3. No functional changes.

## [0.4.0-rc.2] - 2026-03-09

### Added

- `--socket <path>` flag and `RALPH_SOCKET` environment variable for broker socket
  path passthrough. Passed through to all `room` subcommands. (#309)

### Fixed

- Clippy complexity warnings in `loop_runner.rs` resolved. (#364)

## [0.4.0-rc.1] - 2026-03-09

### Added

- `--profile <name>` flag and `RALPH_PROFILE` environment variable for tool profiles.
  Built-in profiles (`Coder`, `Reviewer`, `Coordinator`, `Notion`, `Reader`) define curated
  sets of allowed and disallowed tools. Profiles merge with explicit
  `--allow-tools`/`--disallow-tools`. (#241)

### Fixed

- Token re-join after broker restart — ralph now detects auth failures and automatically
  re-runs `room join` to obtain a fresh token. (#247, #248)

### Changed

- Removed EOF workaround from `/set_status` command dispatch. (#250)

## [0.3.0-rc.2] - 2026-03-08

### Added

- `--disallow-tools <tools>` flag and `RALPH_DISALLOWED_TOOLS` environment variable for
  hard-blocking tools from the claude session (passed as `--disallowedTools`). Supports
  granular patterns like `Bash(python3:*)`. Empty by default. (#242)
- Automatic `/set_status` updates at loop milestones: before claude spawn, on context
  restart, on claude error, and on shutdown. Handles #234 EOF gracefully. (#243)

### Fixed

- Strip `CLAUDECODE` and `CLAUDE_CODE_ENTRY_POINT` environment variables from the child
  claude process to prevent the nested-session guard from blocking startup. (#239)

## [0.3.0-rc.1] - 2026-03-07

### Added

- Environment variable configuration: `RALPH_ROOM`, `RALPH_USERNAME`, `RALPH_MODEL`,
  `RALPH_ISSUE` as alternatives to CLI positional args and flags. (#222)
- `RALPH_ALLOWED_TOOLS` environment variable for setting the tool allow list without
  the CLI flag. Supports `none` to disable restrictions. (#222)
- Safe default tool set (`Read`, `Glob`, `Grep`, `WebSearch`, `Bash(room *)`,
  `Bash(git status)`, `Bash(git log)`, `Bash(git diff)`) applied when neither
  `--allow-tools` nor `RALPH_ALLOWED_TOOLS` is set. (#221)

## [0.2.0] - 2026-03-06

### Added

- `--allow-tools <tools>` flag — comma-separated tool allow list passed through
  as `--allowedTools` to the claude subprocess. (#213)
- `--personality <file>` flag — file contents prepended before all prompt content,
  enabling per-agent personality without replacing the default system prompt. (#215)
- Integration tests with mock binaries. (#211)
- Crate-level README for crates.io. (#218)

### Fixed

- `-v` (lowercase) now works as an alias for `--version`, matching `-V`. (#214)

## [0.1.0] - 2026-03-06

### Added

- Initial release as a Rust port of the `ralph-room.sh` shell script. (#199, #206)
- Autonomous agent loop: join room, poll messages, build prompt, spawn `claude -p`,
  monitor token usage, restart on context exhaustion.
- Context monitoring with configurable threshold (`CONTEXT_THRESHOLD`, default 80%)
  and limit (`CONTEXT_LIMIT`, default 200k tokens).
- Progress file persistence at `/tmp/room-progress-<issue>.md` for cross-session
  state recovery.
- `--tmux` flag to run in a detached tmux session.
- `--dry-run` flag to print the prompt and exit without spawning claude.
- `--model`, `--issue`, `--max-iter`, `--cooldown`, `--prompt`, `--add-dir` flags.
- Logging to stderr and `/tmp/ralph-room-<username>.log`.
- 81 unit tests + 8 integration tests.

[Unreleased]: https://github.com/knoxio/room/compare/v3.0.0-rc.6...HEAD
[0.4.0-rc.6]: https://github.com/knoxio/room/compare/v3.0.0-rc.5...v3.0.0-rc.6
[0.4.0-rc.5]: https://github.com/knoxio/room/compare/v3.0.0-rc.3...v3.0.0-rc.5
[0.4.0-rc.3]: https://github.com/knoxio/room/compare/v3.0.0-rc.2...v3.0.0-rc.3
[0.4.0-rc.2]: https://github.com/knoxio/room/compare/v3.0.0-rc.1...v3.0.0-rc.2
[0.4.0-rc.1]: https://github.com/knoxio/room/compare/v2.1.0-rc.2...v3.0.0-rc.1
[0.3.0-rc.2]: https://github.com/knoxio/room/compare/v2.1.0-rc.1...v2.1.0-rc.2
[0.3.0-rc.1]: https://github.com/knoxio/room/compare/v2.0.1...v2.1.0-rc.1
[0.2.0]: https://github.com/knoxio/room/compare/v2.0.0...v2.0.1
[0.1.0]: https://github.com/knoxio/room/releases/tag/v2.0.0
