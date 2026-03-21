# Changelog

All notable changes to `room-plugin-taskboard` will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- `/taskboard approve` is now context-aware — approves plan (Planned → InProgress) when task
  is planned, or approves review (ReviewClaimed → Finished) when task has a claimed reviewer.
  For review approval, only the reviewer or host can approve. (#843)

### Added

- `/taskboard reject <task-id> [reason]` subcommand — reviewer sends task back to implementer.
  Transitions ReviewClaimed → InProgress, clears reviewer/approved fields, sets rejection
  reason in notes. Only reviewer or host can reject. Emits `TaskRejected` event. (#845)
- `/taskboard review_claim` subcommand — claim reviewer role on tasks in AwaitingReview status.
  Sets reviewer field and restarts lease timer. Emits `ReviewClaimed` event. (#844)
- `/taskboard qa-queue` subcommand — filtered view showing only tasks in AwaitingReview status,
  helping QA reviewers find tasks ready for review (#846)
- `/taskboard mine` subcommand — filter tasks to show only those assigned to the calling user (#827)
- `/taskboard history` subcommand — shows finished and cancelled tasks (#824)

## [3.5.1] - 2026-03-16

## [3.5.0] - 2026-03-15

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
