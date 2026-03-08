# Changelog

All notable changes to `room-cli` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

For changes prior to the workspace restructure (v0.1.2 through v1.0.2), see the
[root CHANGELOG](../../CHANGELOG.md).

## [Unreleased]

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

[Unreleased]: https://github.com/knoxio/room/compare/v2.0.1...HEAD
[2.0.1]: https://github.com/knoxio/room/compare/v2.0.0...v2.0.1
[2.0.0]: https://github.com/knoxio/room/releases/tag/v2.0.0
