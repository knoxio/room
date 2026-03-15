# Changelog

All notable changes to `room-protocol` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `plugin::abi` module with C ABI types for dynamic plugin loading:
  `PluginDeclaration` (`#[repr(C)]`), `CreateFn`, `DestroyFn` type aliases,
  symbol name constants, `config_from_raw` helper
- `declare_plugin!` macro for generating cdylib ABI entry points with
  `cfg_attr`-gated `#[no_mangle]` (via `cdylib-exports` feature flag)
- `Plugin::on_message()` lifecycle hook — called after every broadcast, default no-op

## [3.4.0] - 2026-03-15

## [3.3.0] - 2026-03-15

## [3.2.0] - 2026-03-13

### Added

- `plugin` module with framework types for external plugin development:
  `Plugin` trait, `CommandInfo`, `ParamSchema`, `ParamType`, `PluginResult`,
  `CommandContext`, `RoomMetadata`, `UserInfo`, `BoxFuture`, and new
  `MessageWriter`/`HistoryAccess` async traits. External crates can implement
  plugins by depending only on `room-protocol`. (#558)
- Plugin versioning: `version()`, `api_version()`, `min_protocol()` methods on `Plugin`
  trait with backward-compatible defaults. `PLUGIN_API_VERSION` and `PROTOCOL_VERSION`
  constants for broker-side compatibility checking. (#512)
- `TeamAccess` trait in `plugin` module — read-only team membership queries for plugins.
  `team_exists(team)` and `is_member(team, user)` methods. `CommandContext` gains an
  optional `team_access` field (`None` in standalone mode, `Some` in daemon mode). (#516)

## [3.1.0] - 2026-03-13

### Added

- `EventType` enum (`#[non_exhaustive]`) — typed event categories for structured
  event filtering: task lifecycle, status changes, review requests. (#430)
- `Message::Event` variant — carries `event_type`, `content`, and optional `params`
  (structured JSON). Broadcast like any other message; no filtering in this release. (#430)
- `make_event()` constructor for building typed events. (#430)
- `EventType::FromStr` impl — parse event type names from strings. (#430)
- `EventType` `Ord`/`PartialOrd` impls — enables use in `BTreeSet`. (#430)
- `EventFilter` enum — `All`, `None`, `Only { types }` for per-user event type
  filtering. Serde, Display, FromStr, Default(All). (#430)

## [3.0.0-rc.6] - 2026-03-10

Version bump to stay in sync with room-cli 3.0.0-rc.6. No protocol changes.

## [3.0.0-rc.5] - 2026-03-10

Version bump to stay in sync with room-cli 3.0.0-rc.5. No protocol changes.

## [3.0.0-rc.3] - 2026-03-09

Version bump to stay in sync with room-cli 3.0.0-rc.3. No protocol changes.

## [3.0.0-rc.2] - 2026-03-09

### Added

- `Message::is_visible_to()` — centralised DM visibility check, replacing scattered
  filter patterns across the codebase. (#352)
- `SubscriptionTier` enum (`Full`, `MentionsOnly`, `Unsubscribed`) for tiered room
  subscriptions. (#323)
- `format_message_id()` and `parse_message_id()` helpers for structured message ID
  handling. (#321)

### Changed

- `dm_room_id()` now returns an error when both users are the same. (#296)

## [3.0.0-rc.1] - 2026-03-09

### Added

- `parse_mentions()` — extract `@username` mentions from text. (#256)
- `Message::content()` and `Message::mentions()` accessors. (#256)
- `RoomConfig` and `RoomVisibility` types for room access control. (#253)
- `dm_room_id()` — deterministic room ID for DM pairs.

### Changed

- Major version bump to v3 for breaking wire format additions (new types and
  visibility model).

## [2.1.0-rc.2] - 2026-03-08

Version bump to stay in sync with room-cli 2.1.0-rc.2. No functional changes.

## [2.1.0-rc.1] - 2026-03-07

Version bump to stay in sync with room-cli 2.1.0. No functional changes.

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
