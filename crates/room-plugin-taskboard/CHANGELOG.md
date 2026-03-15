# Changelog

All notable changes to `room-plugin-taskboard` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- C ABI entry points for dynamic loading via `declare_plugin!` macro
- `cdylib-exports` feature flag for `#[no_mangle]` symbol export
- `crate-type = ["cdylib", "rlib"]` for shared library builds
- JSON config parsing for dynamic plugin instantiation

## [3.4.0] - 2026-03-15

## [3.3.0] - 2026-03-15

## [3.2.0] - 2026-03-13

## [3.1.0] - 2026-03-13

### Added

- Initial extraction from room-cli as independent workspace crate.
- Team-restricted task claims: `/taskboard post --team <name>` restricts claiming
  and assignment to team members.
