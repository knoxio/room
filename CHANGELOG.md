# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-03-05

### Added

- **TUI: room name in title bar** — the message panel now displays the actual room ID
  instead of the hardcoded string `"room"`.
- **TUI: per-user colors** — each username is assigned a distinct color from a 10-color
  palette using a deterministic hash, making it easy to distinguish speakers at a glance.
  The `[dm]` prefix retains magenta; only the sender name adopts the per-user color.
- **TUI: word-wrap long messages** — messages that exceed the terminal width now wrap at
  word boundaries and indent continuation lines to align under the content start. Hard
  splits are used for words longer than the available width. Unicode-aware: uses character
  counts rather than byte lengths throughout.
- **Docs: autonomous agent polling loop** — `CLAUDE.md`, `README.md`, and the
  `room-coordination` skill now document the `run_in_background` + `TaskOutput` pattern
  for agents that need to stay resident without human re-prompting. Covers self-message
  filtering, file-based poll output (avoiding `$()` hook blocking), and cursor persistence.

### Changed

- **Deps: ratatui 0.29 → 0.30, crossterm 0.28 → 0.29** — picks up upstream fixes and
  bumps `lru` from 0.12.5 to 0.16.3, resolving a soundness advisory in `IterMut`.
- **Refactor: `handle_client` arguments** — shared broker state (`clients`, `status_map`,
  `host_user`, `chat_path`, `room_id`) is now bundled into a `RoomState` struct, satisfying
  the `clippy::too_many_arguments` lint without suppression.

### Fixed

- `cargo fmt` and `cargo clippy` now pass cleanly on all source files.

## [0.1.4] - 2026-03-05

### Changed

- Added `cargo-release` configuration (`release.toml`).

## [0.1.3] - 2026-03-05

### Fixed

- Allow dirty working tree during `cargo publish` (Cargo.lock not ignored by crates.io).

## [0.1.2] - 2026-03-04

### Added

- Initial public release with `room send` / `room poll` one-shot subcommands.
- Cursor file at `/tmp/room-<id>-<username>.cursor` for stateless incremental polling.
- `SEND:` handshake in the broker for one-shot sends — no join/leave events emitted.
- Direct messages (`dm` wire type) delivered only to sender, recipient, and broker host.
- `/set_status` and `/who` broker commands.
- TUI built with ratatui (split-pane, scroll, key bindings).
- `--agent` mode for long-lived processes with JSON stdin/stdout.
- Claude Code plugin with `room-coordination` skill and `/room:check`, `/room:send` commands.

[Unreleased]: https://github.com/knoxio/room/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/knoxio/room/compare/v0.1.4...v0.2.0
[0.1.4]: https://github.com/knoxio/room/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/knoxio/room/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/knoxio/room/releases/tag/v0.1.2
