# Changelog

All notable changes to `room-daemon` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Remove static `room-plugin-agent` dependency — agent plugin now loaded dynamically
  from `~/.room/plugins/` via the libloading system. Part of room-ralph extraction to
  knoxio/room-ralph.

## [3.5.1] - 2026-03-16

## [3.5.0] - 2026-03-15

### Added

- `PluginRegistry::notify_message()` — broadcasts message observations to all plugins
- Broker session calls `notify_message` after every successful broadcast/DM persist
- `room_plugins_dir()` path helper for `~/.room/plugins/` directory. (#745)
- Dynamic plugin loader (`plugin/loader.rs`) — scans `~/.room/plugins/` for `.so`/`.dylib`
  files and loads them via the C ABI entry points. Validates API version and protocol
  compatibility before instantiation. (#744)

## [3.4.0] - 2026-03-15

## [3.3.0] - 2026-03-15

## [3.2.0] - 2026-03-13

## [3.1.0] - 2026-03-13

### Added

- Initial extraction from room-cli as independent workspace crate.
- Broker, plugin registry, user registry, history, and path resolution modules.
