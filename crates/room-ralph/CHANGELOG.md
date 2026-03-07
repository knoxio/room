# Changelog

All notable changes to `room-ralph` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
[0.2.0]: https://github.com/knoxio/room/compare/v2.0.0...v2.0.1
[0.1.0]: https://github.com/knoxio/room/releases/tag/v2.0.0
