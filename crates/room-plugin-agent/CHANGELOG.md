# Changelog

All notable changes to `room-plugin-agent` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- Agent stale detection: `HealthStatus` enum (Healthy/Stale/Exited) with configurable threshold (default 5 min)
- `/agent list` now shows health column based on last message activity
- `on_message` hook tracks last-seen timestamp for spawned agents

## [3.2.0] - 2026-03-13

### Added

- Initial release: AgentPlugin with /agent spawn, list, stop, logs commands
- Personality registry with TOML overrides and name pools
- /spawn command for personality-based agent shortcuts
- Structured plugin responses with machine-readable data field
- TUI command palette autocomplete for /agent and /spawn
