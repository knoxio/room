# Changelog

All notable changes to `room-daemon` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `PluginRegistry::notify_message()` — broadcasts message observations to all plugins
- Broker session calls `notify_message` after every successful broadcast/DM persist

## [3.4.0] - 2026-03-15

## [3.3.0] - 2026-03-15

## [3.2.0] - 2026-03-13

## [3.1.0] - 2026-03-13

### Added

- Initial extraction from room-cli as independent workspace crate.
- Broker, plugin registry, user registry, history, and path resolution modules.
