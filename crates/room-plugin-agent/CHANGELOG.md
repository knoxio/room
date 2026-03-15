# Changelog

All notable changes to `room-plugin-agent` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- C ABI entry points for dynamic loading via `declare_plugin!` macro
- `cdylib-exports` feature flag for `#[no_mangle]` symbol export
- `crate-type = ["cdylib", "rlib"]` for shared library builds
- JSON config parsing for dynamic plugin instantiation
- Agent stale detection: `HealthStatus` enum (Healthy/Stale/Exited) with configurable threshold (default 5 min)
- `/agent list` now shows health column based on last message activity
- `on_message` hook tracks last-seen timestamp for spawned agents
- `PersonalityError` enum with `Io`, `Parse`, and `Validation` variants for typed error reporting
- `Personality::validate()` method — checks name, description, model, and name_pool entries
- TOML schema validation on personality load — malformed files now produce clear error messages
- 13 new tests covering all validation and error paths

### Changed

- `load_personality_toml` returns `Result<Personality, PersonalityError>` instead of `Option<Personality>`
- `resolve_personality` returns `Result<Option<Personality>, PersonalityError>` — propagates errors from malformed user TOML files instead of silently falling through to builtins

## [3.2.0] - 2026-03-13

### Added

- Initial release: AgentPlugin with /agent spawn, list, stop, logs commands
- Personality registry with TOML overrides and name pools
- /spawn command for personality-based agent shortcuts
- Structured plugin responses with machine-readable data field
- TUI command palette autocomplete for /agent and /spawn
