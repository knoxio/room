# Contributing

## Prerequisites

- [Rust stable](https://rustup.rs/) (the current MSRV is whatever `Cargo.toml` specifies)
- `cargo` in `$PATH`

No other runtime dependencies. The broker and all tests run against Unix domain sockets on the local filesystem.

## Development setup

```bash
git clone https://github.com/knoxio/room
cd room
cargo build
cargo test
```

All integration tests spin up a real broker process against a temporary socket and chat file — no mocks, no fakes. They clean up after themselves. Tests can be run in parallel by default.

## Pre-push checklist

Run all four in order before every `git push`. CI enforces all of them:

```bash
cargo check                  # catches syntax/type errors and unresolved merge conflict markers
cargo fmt                    # reformats code; commit the result if anything changed
cargo clippy -- -D warnings  # fix root causes, never suppress with allow(...)
cargo test                   # all tests must pass
```

Order matters: `cargo check` must come first because conflict markers and type errors can confuse the formatter. `cargo fmt` must run after any clippy-driven rewrites, since collapsing a match arm can push a line over the line-length limit.

**Never push if any step fails.** Fix the root cause; do not use `#[allow(...)]`, `// eslint-disable`, or `--no-verify` to bypass the checks.

## Branch naming

Use the issue number and a short description:

```
feat/issue-42-add-auth
fix/issue-69-tui-exit
docs/issue-84-broker-internals
```

One branch per issue. Keep branches small and focused.

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/) style:

```
feat(broker): add outbound shutdown select arm
fix(tui): detect Disconnected in drain loop
docs: add wire-format reference
chore: release 0.7.0
```

Subject line: imperative mood, ≤72 characters, no trailing period.

Body: explain *why*, not just *what*. Reference the issue number.

Write the commit message to a file and use `git commit -F` to avoid shell quoting issues with multi-line messages:

```bash
# Write message to /tmp/commit_msg.txt, then:
git commit -F /tmp/commit_msg.txt
```

Do not add `Co-Authored-By` or AI tool references to commits.

## PR discipline

1. **Announce before touching any file.** Send a message in the room before you start: what you're working on, which files you'll touch, your implementation approach.
2. **Wait for acknowledgement** before writing code if another agent may be working on the same files.
3. **Announce before pushing** — even for fix commits, rebases, and CI failures.
4. **Do not open a PR without announcing it** in the room first.
5. **Do not merge** — the BA reviews and merges all PRs.

The project room is the coordination layer. Use it.

## Test requirements

New behaviour must have tests. The test split:

- **Unit tests** (`src/*.rs` via `#[cfg(test)]`): test individual functions and logic in isolation.
- **Integration tests** (focused modules under `tests/` — auth.rs, broker.rs, daemon.rs, oneshot.rs, rest_query.rs, room_lifecycle.rs, scripted.rs, ws.rs, ws_smoke.rs): test the full broker stack with a live `UnixListener`. Use the helpers in `tests/common/mod.rs` (`TestBroker::start`, `TestClient::connect`, `send_text`, `send_json`, `recv`, `recv_until`, etc.) to keep tests concise.

When writing integration tests:

- Start a fresh broker with a unique socket and chat path per test to avoid collisions.
- Test the actual wire behaviour — connect as a real client, send real bytes, assert on real JSON output.
- Avoid `sleep` in tests; use bounded `read_line` with a timeout instead. The existing `exit_causes_broker_to_close_client_connections` test is a good reference for timeout-based assertions.

## Key invariants

These must be preserved by all changes:

- **Broker is the sole writer** to the chat file. Clients must never write to it directly.
- **`Message` is a flat internally-tagged enum** — do not add `#[serde(flatten)]` inside a `#[serde(tag)]` enum. It breaks deserialization silently.
- **All file I/O uses `std::fs` or `spawn_blocking`** — `tokio::fs` wrappers can be cancelled on runtime shutdown.
- **`src/lib.rs` must export any new modules** so integration tests can import them.
- **Sequence numbers are broker-assigned** — clients must not set `seq`. The `AtomicU64` counter in `RoomState` is the single source of truth.

## Where to ask

Open a GitHub issue, or if the project room is running, join it:

```bash
room join <your-username>
room subscribe <room-id>
room watch <room-id> -t <token>
```
