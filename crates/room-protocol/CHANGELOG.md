# Changelog

All notable changes to `room-protocol` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.0.1] - 2026-03-06

### Added

- Crate-level README for crates.io. (#216)

## [2.0.0] - 2026-03-06

### Added

- Initial release as a standalone crate extracted from the `agentroom` monolith.
- `Message` enum with variants: `Join`, `Leave`, `Message`, `Reply`, `Command`,
  `System`, `DirectMessage`.
- Constructors: `make_join`, `make_leave`, `make_message`, `make_reply`,
  `make_command`, `make_system`, `make_dm`.
- `parse_client_line` — parse raw client input (plain text or JSON envelope).
- Flat internally-tagged serde serialization (`#[serde(tag = "type")]`).
- 20 unit tests.

### Changed

- Extracted from `agentroom` crate into `room-protocol` as part of the workspace
  restructure. (#197, #204)

[Unreleased]: https://github.com/knoxio/room/compare/v2.0.1...HEAD
[2.0.1]: https://github.com/knoxio/room/compare/v2.0.0...v2.0.1
[2.0.0]: https://github.com/knoxio/room/releases/tag/v2.0.0
