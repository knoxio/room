# room — Agent Coordination Guide

## What is `room`?

`room` is a CLI tool that lets multiple Claude agents and humans share a group chat over
Unix domain sockets. It is the coordination layer for this project. When you are working
on a feature, you are expected to join the shared room and stay connected for the duration
of your work.

## Communicating from Claude Code (sequential tool model)

Claude Code cannot block on a persistent socket connection. Use the one-shot subcommands.

### Session setup (once per agent per broker restart)

```bash
# Register your username with the broker — writes a token to /tmp/room-<id>-<username>.token
room join <room-id> <your-username>
# Output: {"type":"token","token":"<uuid>","username":"<your-username>"}
```

Save the token from the output. Pass it explicitly with `-t` on every subsequent command. If the broker restarts, re-run `room join` to get a new token.

### One-shot commands

```bash
# Send a message — prints the broadcast JSON and exits
room send <room-id> -t <token> your message here

# Send a direct message to a specific user
room send <room-id> -t <token> --to <recipient> your message here

# Check for new messages since last poll — prints NDJSON and exits
room poll <room-id> -t <token>

# Check messages since a specific ID (overrides stored cursor)
room poll <room-id> -t <token> --since <message-id>
```

The cursor is stored at `/tmp/room-<id>-<username>.cursor` (username resolved from token). A second `room poll` with no `--since` returns only messages that arrived after the first call.

### Staying resident (autonomous loop)

Use `room watch` — it blocks until a foreign message arrives, then exits. No external script needed.

```bash
room watch <room-id> -t <token> --interval 5
```

Run it with `run_in_background=true` and `timeout=600000`. Block on `TaskOutput` — when a message arrives the task completes, you act, send a reply via `room send`, then re-launch `room watch`.

### Typical loop

```bash
# 0. Join the room (once per broker lifetime) — save the token from the output
room join myroom feat-myfeature
# → {"type":"token","token":"<uuid>","username":"feat-myfeature"}
# Export for convenience:
TOKEN=<uuid>

# 1. Announce yourself and propose your plan
room send myroom -t $TOKEN "starting #42. plan: add Foo struct to src/broker.rs, wire into handle_message(). no changes to wire format."

# 2. Poll for objections before writing any code (~30s wait)
room poll myroom -t $TOKEN
# If someone objects or flags a conflict — resolve it here before continuing.

# 3. Mid-implementation checkpoints — after reading the target file, after first draft
room send myroom -t $TOKEN "read src/broker.rs. adding Foo in the handler section."
# ... write code ...
room send myroom -t $TOKEN "first draft done. running tests."

# 4. Before opening a PR — poll for anything missed
room poll myroom -t $TOKEN
room send myroom -t $TOKEN "opening PR for #42. modified: src/broker.rs, tests/integration.rs"

# 5. Before pushing a fix commit (review feedback, CI failure, conflict) — announce first
room send myroom -t $TOKEN "fixing clippy error on PR #42, hold review"
# ... fix ...
room send myroom -t $TOKEN "fix pushed"
```

## Wire format

Every message is a JSON object with a `type` field:

```json
{"type":"join","id":"...","room":"...","user":"alice","ts":"..."}
{"type":"message","id":"...","room":"...","user":"bob","ts":"...","content":"hello"}
{"type":"leave","id":"...","room":"...","user":"alice","ts":"..."}
{"type":"command","id":"...","room":"...","user":"bob","ts":"...","cmd":"claim","params":["task"]}
{"type":"reply","id":"...","room":"...","user":"bob","ts":"...","reply_to":"<msg-id>","content":"ack"}
```

To send structured input via `--agent` stdin or `room send`, plain text is also accepted.

## Message content

**Do not prefix messages with your own username.** Every message in the wire format already
carries the sender's identity (`"user": "<username>"`), and the TUI displays it on every
line. A `username:` prefix is redundant and clutters the chat.

```bash
# Wrong — do not do this:
room send myroom -t $TOKEN "feat-myfeature: opening PR for #42"

# Correct:
room send myroom -t $TOKEN "opening PR for #42"
```

## Expected behaviour

### On starting work
1. Join the room if you don't have a token yet: `room join <room-id> <username>` — save the token UUID from the output.
2. Poll for recent history: `room poll <room-id> -t <token>`
3. Announce yourself and **propose your plan**: who you are, what branch you are on, which
   files you intend to modify, and your implementation approach in 2–3 sentences.
4. Poll again after ~30 seconds. If anyone objects or flags a conflict, resolve it before
   writing any code. Silence means proceed.

### During work
- **Claim tasks before starting them**: `room send <room-id> -t <token> /claim <description>`
- **Announce before touching any file** — even on fix commits or rebases. Send the intent
  message first, then do the work. Never start silently.
  ```
  "fixing conflict on PR #30, hold review"
  "about to modify src/tui.rs — adding palette overlay only, not touching input rendering"
  ```
- **Fix commits, CI failures, and rebases require the same announce-first discipline.**
  Do not silently push a fix. Announce before you start, then again when pushed:
  ```
  "clippy error on PR #42 — fixing, hold review"
  "rebase conflict on PR #38, resolving now"
  "addressing review feedback on PR #55 — touching broker.rs only"
  ```
  After the fix is pushed: `"fix pushed, PR #42 ready for re-review"`
- **Poll and send a milestone update at natural breakpoints:**
  - After reading the target file (before writing any code)
  - After completing a first working draft (before running tests)
  - Before opening or updating a PR
- **Broadcast blockers.** If you are stuck or need a decision, say so clearly. Do not silently stall.

### Coordination rules
- **One agent per file at a time.** If you need to modify a file that another agent has
  claimed, ask them first.
- **Schema changes need consensus.** If your feature requires adding a new message type or
  changing the wire format, announce the proposed change and wait for agreement before
  implementing.
- **The human (host) has final say.** If the host sends a message, treat it as a directive.
  Stop what you are doing, acknowledge it, and follow the instruction.
- **Do not merge or push without announcing it** in the room first.

### On completion
1. Announce that your work is done and summarise what changed.
2. State which files were modified.
3. Note any decisions or trade-offs the other agents or the human should be aware of.
4. Include `Closes #<issue-number>` in the PR description so the issue auto-closes on merge.

## Workspace structure

This is a cargo workspace. The root `Cargo.toml` defines `[workspace]` with `members = ["crates/*"]`.
The room crate (package `agentroom`) lives at the workspace root. Future crates (e.g. `room-agent`,
`room-plugins`) go under `crates/`.

```
Cargo.toml          — workspace root + room package
Cargo.lock          — shared across all workspace members
crates/             — future workspace members (room-agent, etc.)
src/                — room crate source
tests/              — room integration tests
```

## Codebase overview

```
src/
  main.rs              — CLI parsing, subcommand dispatch (join / send / poll / watch)
  lib.rs               — Re-exports all modules (required for integration tests)
  client.rs            — Connects to broker, runs TUI or agent mode
  message.rs           — Wire format enum, constructors, parse_client_line
  history.rs           — NDJSON load/append
  broker/
    mod.rs             — Accept loop, handle_client, handle_oneshot_send
    state.rs           — RoomState struct and type aliases (ClientMap, StatusMap, etc.)
    auth.rs            — Token issuance (issue_token) and validation (validate_token)
    commands.rs        — Unified command routing (route_command, handle_admin_cmd)
    fanout.rs          — broadcast_and_persist, dm_and_persist
  oneshot/
    mod.rs             — Re-exports and subcommand dispatch
    transport.rs       — Socket connect, send_message, send_message_with_token
    token.rs           — Token file I/O, cursor read/write, cmd_join
    poll.rs            — poll_messages, pull_messages, cmd_poll, cmd_pull, cmd_watch
  tui/
    mod.rs             — Main run() loop and TUI state
    input.rs           — InputState, handle_key, Action enum
    render.rs          — format_message, wrap_words, rendering helpers
    widgets.rs         — CommandPalette, MentionPicker
tests/
  integration.rs       — Integration tests against a live broker
```

Key invariants to preserve:
- **Broker is the sole writer** to the chat file. Never write to it from a client.
- **`Message` is a flat internally-tagged enum** — do not use `#[serde(flatten)]` with
  `#[serde(tag)]`, it breaks deserialization.
- **All file IO uses `std::fs` (synchronous) or explicit `spawn_blocking`** — `tokio::fs`
  wrappers get cancelled on runtime shutdown.
- **`src/lib.rs` must export any new modules** so integration tests can import them.
- **All tests must pass** before committing: `cargo test`.

## Pre-push checklist

Run all four in order before every `git push`. CI enforces all of them and will fail if any
step is skipped. Run them in this order — each step can invalidate the previous one.

```bash
bash scripts/pre-push.sh     # runs all four steps below in order
```

Or manually:

```bash
cargo check                  # catches syntax/type errors incl. unresolved conflict markers
cargo fmt                    # reformats code; commit the result if anything changed
cargo clippy -- -D warnings  # fix root causes, never suppress
cargo test                   # all tests must pass
```

To install as a git hook: `ln -sf ../../scripts/pre-push.sh .git/hooks/pre-push`

- `cargo check` must come first — it catches unresolved merge conflict markers (`<<<<<<<`)
  and type errors that `fmt` and `clippy` will not report cleanly.
- `cargo fmt` must run *after* any clippy-driven rewrites, since collapsing a nested `if`
  can push a line over the formatter's line-length limit.
- Never use `#[allow(...)]` or `// eslint-disable` to silence warnings. Fix the root cause.

## Running tests

```bash
cargo test
```

All tests must remain green. Add tests for any new behaviour.

## Baseline test count

**Current baseline: 251 tests (195 unit + 56 integration)**

Every PR that adds functionality must also add tests. The test count must never decrease
without explicit justification in the PR description. If you remove tests, explain why
and ensure coverage is not regressed.

## Release process

Only the human (joao) or the BA agent authorises a release. Do not cut a release without
explicit instruction.

```bash
# 1. Ensure master is up to date and all tests pass
git checkout master && git pull
cargo check && cargo fmt --check && cargo clippy -- -D warnings && cargo test

# 2. Cut the release (updates Cargo.toml, commits, tags, pushes tag + master)
#    release.toml has publish=false — crates.io is handled by CI
cargo release <version> --execute

# 3. Verify the release CI workflow triggered within ~30 s
gh run list --workflow release.yml --limit 3
# If the release.yml run does NOT appear, trigger it manually:
gh workflow run release.yml --ref v<version>
# (release.yml must have a workflow_dispatch trigger for this to work)

# 4. Wait for CI to finish — it builds binaries and publishes to crates.io
gh run watch   # or check GitHub Actions tab
```

### Rules
- **Never run `gh release create` manually.** The release CI creates the GitHub release and
  attaches cross-platform binaries. A manual release has no binaries and can confuse CI.
- **Never run `cargo publish` manually.** CI does it. If CI fails to trigger, fix the
  trigger first, then let CI publish.
- **Never bump the version without a code change** just to fix a botched release. Fix the
  underlying issue instead.
- If CI is suppressed (e.g. tag pushed alongside a branch-protection bypass), delete and
  re-push the tag, or use `gh workflow run` if workflow_dispatch is enabled.
