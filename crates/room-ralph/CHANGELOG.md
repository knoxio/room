# Changelog

All notable changes to `room-ralph` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0-rc.2] - 2026-03-08

### Added

- `--disallow-tools <tools>` flag and `RALPH_DISALLOWED_TOOLS` environment variable for
  hard-blocking tools from the claude session (passed as `--disallowedTools`). Supports
  granular patterns like `Bash(python3:*)`. Empty by default. (#242)
- Automatic `/set_status` updates at loop milestones: before claude spawn, on context
  restart, on claude error, and on shutdown. Handles #234 EOF gracefully. (#243)
- `--profile <name>` flag and `RALPH_PROFILE` environment variable for tool profiles.
  Built-in profiles (`Coder`, `Reviewer`, `Coordinator`, `Notion`, `Reader`) define curated
  sets of allowed and disallowed tools. Profiles merge with explicit
  `--allow-tools`/`--disallow-tools`. (#241)

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

[Unreleased]: https://github.com/knoxio/room/compare/v2.0.1...HEAD
[0.3.0-rc.2]: https://github.com/knoxio/room/compare/v2.0.1...HEAD
[0.3.0-rc.1]: https://github.com/knoxio/room/compare/v2.0.1...HEAD
[0.2.0]: https://github.com/knoxio/room/compare/v2.0.0...v2.0.1
[0.1.0]: https://github.com/knoxio/room/releases/tag/v2.0.0
