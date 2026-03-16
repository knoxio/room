# Changelog

All notable changes to `room-plugin-agent` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed

- `/agent stop` now kills the entire process group (ralph + all child claude
  processes) instead of only the direct child. Spawned agents use `setsid()` to
  create their own process group, and stop uses `kill(-pgid, SIGTERM)` to
  terminate all descendants. Prevents orphaned claude processes from continuing
  to respond after stop. (#777)
- `/spawn` now passes `--personality` instead of `--prompt` to room-ralph, preserving
  the default system context (room send/poll commands and token). Previously, the
  personality template text was passed as `--prompt` (which expects a file path),
  causing the agent to lose all room communication instructions. (#770)

## [3.5.0] - 2026-03-15

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
- `scan_personalities_dir()` — loads all `.toml` files from `~/.room/personalities/`, collecting per-file errors
- `merge_personality()` — merges user overrides into built-in base with deny-wins semantics for tool restrictions
- `all_personalities()` — returns merged map of built-ins + user overrides
- 25 new tests covering validation, scanning, merging, and error paths

### Changed

- `load_personality_toml` returns `Result<Personality, PersonalityError>` instead of `Option<Personality>`
- `resolve_personality` returns `Result<Option<Personality>, PersonalityError>` — propagates errors from malformed user TOML files instead of silently falling through to builtins
- `resolve_personality` now merges user overrides with built-in defaults instead of full replacement
- `all_personality_names` now delegates to `all_personalities` for consistent behavior

## [3.2.0] - 2026-03-13

### Added

- Initial release: AgentPlugin with /agent spawn, list, stop, logs commands
- Personality registry with TOML overrides and name pools
- /spawn command for personality-based agent shortcuts
- Structured plugin responses with machine-readable data field
- TUI command palette autocomplete for /agent and /spawn
