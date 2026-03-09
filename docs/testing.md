# Testing

`room` has two test suites: **unit tests** (inside `src/`) and **integration
tests** (`tests/integration.rs`) that run a real broker over a Unix socket.

---

## Running the tests

```bash
cargo test
```

This runs all unit and integration tests. The integration tests spin up
real broker processes against temporary sockets, so they require no external
setup.

Pre-push, always run the full checklist in order:

```bash
cargo check                  # catches syntax/type errors first
cargo fmt                    # reformat; commit the result if changed
cargo clippy -- -D warnings  # fix root causes, never suppress
cargo test                   # all tests must pass
```

Run a single test by name:

```bash
cargo test test_name
```

Run all tests whose names contain a substring:

```bash
cargo test broadcast
```

---

## Test structure

```
crates/room-protocol/src/
  lib.rs                — unit tests for Message serde, parse_client_line, parse_mentions

crates/room-cli/src/
  history.rs            — unit tests for NDJSON load/append
  broker/auth.rs        — unit tests for token issuance, persistence, join permissions
  broker/commands.rs    — unit tests for route_command, validate_params, built-in commands
  broker/fanout.rs      — unit tests for broadcast_and_persist, dm_and_persist
  plugin/mod.rs         — unit tests for PluginRegistry, builtin_command_infos
  plugin/help.rs        — unit tests for /help output
  plugin/stats.rs       — unit tests for /stats output
  tui/input.rs          — unit tests for handle_key, cursor, key bindings
  tui/render.rs         — unit tests for wrap_words, format_message
  tui/widgets.rs        — unit tests for CommandPalette, MentionPicker
  oneshot/token.rs      — unit tests for token file I/O, cursor read/write
  oneshot/poll.rs       — unit tests for DM filter, cursor advancement, multiplexed poll
crates/room-cli/tests/
  common/mod.rs         — shared test helpers (free_port, wait_for_socket, wait_for_tcp)
  integration.rs        — integration tests against a live broker (UDS + WS + daemon)
  ws_smoke.rs           — end-to-end smoke tests spawning the real binary with --ws-port

crates/room-ralph/src/
  claude.rs             — unit tests for tool resolution, profile merging
  lib.rs                — unit tests for CLI parsing, env var handling
  monitor.rs            — unit tests for context monitoring
  progress.rs           — unit tests for progress file I/O
  prompt.rs             — unit tests for prompt building
  room.rs               — unit tests for room CLI wrapper
crates/room-ralph/tests/
  integration.rs        — integration tests with mock binaries
```

Unit tests live in `#[cfg(test)]` modules at the bottom of each source file.
Integration tests import crates via `room_cli::` or `room_ralph::` and spin
up real broker instances or mock binaries.

---

## Integration test helpers

### `TestBroker`

Starts a broker bound to a temporary socket and waits until the socket is
ready:

```rust
let broker = TestBroker::start("my_test_room").await;
// broker.socket_path — path to the Unix socket
// broker.chat_path   — path to the NDJSON chat file
```

The `TempDir` is kept alive for the duration of the test; the socket and
chat file are cleaned up automatically when `TestBroker` drops.

### `TestClient`

Connects a raw client to the broker and provides helpers for sending and
receiving messages:

```rust
let mut client = TestClient::connect(&broker.socket_path, "alice").await;

// Receive the next message (1 s timeout):
let msg = client.recv().await;

// Wait for a specific message (2 s timeout):
let join_msg = client
    .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
    .await;

// Send a plain-text line:
client.send_text("hello everyone").await;

// Send a JSON envelope:
client.send_json(r#"{"type":"message","content":"hello"}"#).await;
```

### Token auth helpers

For tests that require authentication (send/poll via oneshot):

```rust
let (username, token) = room_cli::oneshot::join_session(&broker.socket_path, "bot")
    .await
    .expect("join_session failed");

let wire = serde_json::json!({"type":"message","content":"hi"}).to_string();
let msg = room_cli::oneshot::send_message_with_token(&broker.socket_path, &token, &wire)
    .await
    .expect("send failed");
```

---

## Writing a new integration test

A typical test follows this pattern:

```rust
#[tokio::test]
async fn my_feature_works() {
    // 1. Start a broker with a unique room ID (avoids socket conflicts).
    let broker = TestBroker::start("t_my_feature").await;

    // 2. Connect one or more clients.
    let mut alice = TestClient::connect(&broker.socket_path, "alice").await;

    // 3. Drain the initial join event so subsequent recvs are clean.
    alice
        .recv_until(|m| matches!(m, Message::Join { user, .. } if user == "alice"))
        .await;

    // 4. Exercise the feature.
    alice.send_text("hello").await;

    // 5. Assert the expected outcome.
    let msg = alice
        .recv_until(|m| matches!(m, Message::Message { user, .. } if user == "alice"))
        .await;
    if let Message::Message { content, .. } = msg {
        assert_eq!(content, "hello");
    }
}
```

**Tips:**

- Use a unique `room_id` per test (e.g. `t_my_feature`) to avoid socket path
  conflicts when tests run in parallel.
- Always drain your own join event before asserting on subsequent messages;
  the broker replays history on connect, which can include messages from
  earlier in the same test run.
- Use `recv_until` rather than `recv` when you need to skip unrelated events
  (joins from other clients, system messages, etc.).
- For timing-sensitive tests involving the broker on an encrypted volume,
  allow generous sleep margins — startup can take >300 ms.

---

## Test coverage targets

- Every new broker behaviour should have at least one integration test that
  exercises it at the wire level (connect → send → assert received).
- Every new TUI helper (wrap function, palette filter, cursor logic) should
  have a unit test in the corresponding `#[cfg(test)]` block.
- Tests should aim to surface real failure modes, not just happy paths.
  Examples: duplicate username rejection, permission-denied on admin commands,
  cursor advancement after poll.
