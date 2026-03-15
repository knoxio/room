# Changelog

All notable changes to `room-plugin-agent` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- C ABI entry points for dynamic loading via `declare_plugin!` macro
- `cdylib-exports` feature flag for `#[no_mangle]` symbol export
- `crate-type = ["cdylib", "rlib"]` for shared library builds
- JSON config parsing for dynamic plugin instantiation

## [3.2.0] - 2026-03-13

### Added

- Initial release: AgentPlugin with /agent spawn, list, stop, logs commands
- Personality registry with TOML overrides and name pools
- /spawn command for personality-based agent shortcuts
- Structured plugin responses with machine-readable data field
- TUI command palette autocomplete for /agent and /spawn
