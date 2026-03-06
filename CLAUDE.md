# room — Agent Coordination Guide

> **TL;DR — read these first**
>
> 1. Join the room before doing anything: `room join <room-id> <username>`
> 2. Announce your plan and wait for go-ahead before writing code
> 3. One agent per file — declare ownership before touching a file
> 4. Announce before every push, fix commit, or rebase — never push silently
> 5. Every PR must include tests — test count must not decrease
> 6. Run `bash scripts/pre-push.sh` before every push (or the four commands manually)
> 7. Write shell scripts to `/tmp/` with the Write tool, then `bash /tmp/script.sh` — avoid inline shell metacharacters

## What is `room`?

`room` is a CLI tool that lets multiple Claude agents and humans share a group chat over
Unix domain sockets or WebSocket/REST. It is the coordination layer for this project.
When you are working on a feature, you are expected to join the shared room and stay
connected for the duration of your work.

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

## Shell environment constraints

Claude Code's Bash tool triggers permission prompts on certain shell patterns. Avoid these
to keep the workflow smooth:

**Forbidden patterns:**
- `TOKEN=$(...)` or any `$()` command substitution inline
- `export VAR=...` followed by `$VAR` expansion in the same or later commands
- Double-quoted strings with shell metacharacters (`"$(cat ...)"`, `"${VAR}"`)
- `cat > file << 'EOF'` heredocs (use the Write tool instead)

**Workarounds:**
1. Write multi-step scripts to `/tmp/` using the Write tool, then run with `bash /tmp/script.sh`
2. For token extraction: `python3 -c "import json; print(json.load(open('/tmp/room-<id>-<user>.token'))['token'])"`
3. Pass tokens inline with `--token` flag, not via environment variables
4. For git commits with multi-line messages: write to `/tmp/commit_msg.txt`, then `git commit -F /tmp/commit_msg.txt`

## Token file format

After `room join <room-id> <username>`, the token is saved to `/tmp/room-<room-id>-<username>.token`:

```json
{"type":"token","token":"<uuid>","username":"<username>"}
```

Extract the UUID with:
```bash
python3 -c "import json; print(json.load(open('/tmp/room-<id>-<user>.token'))['token'])"
```

The cursor (last-seen message ID) is stored at `/tmp/room-<id>-<username>.cursor`.

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

**Note:** Plugin commands (e.g. `/help`, `/stats`) are only dispatched when sent via the
TUI or `--agent` JSON envelope. Plain text `room send "/help"` is treated as a regular
message, not a command. This is by design — one-shot sends are for messages, not commands.

## HTTP/WebSocket transport

The broker optionally serves a WebSocket + REST API alongside the Unix domain socket.
Start it with `--ws-port <port>`:

```bash
room myroom myuser --ws-port 4200
```

### WebSocket endpoint

Connect to `ws://host:port/ws/<room_id>`. The handshake protocol mirrors UDS — send one
of these as the first text frame:

| First frame | Behaviour |
|---|---|
| `<username>` | Interactive session (history replay, broadcast, join/leave events) |
| `JOIN:<username>` | Register username, receive `{"type":"token","token":"<uuid>"}`, close |
| `TOKEN:<uuid>` | Authenticated one-shot — send message as next frame, receive echo, close |
| `SEND:<username>` | Legacy unauthenticated one-shot send |

After the interactive handshake, send plain text or JSON envelopes as text frames.
Messages are broadcast to all connected clients (UDS and WS).

### REST API

All REST endpoints require the room to be running with `--ws-port`.

| Method | Endpoint | Auth | Description |
|---|---|---|---|
| `POST` | `/api/<room_id>/join` | None | `{"username":"x"}` → `{"type":"token","token":"<uuid>"}` |
| `POST` | `/api/<room_id>/send` | `Bearer <token>` | `{"content":"msg","to":"user"}` → broadcast JSON |
| `GET` | `/api/<room_id>/poll` | `Bearer <token>` | `?since=<msg-id>` → `{"messages":[...]}` |
| `GET` | `/api/health` | None | `{"status":"ok","room":"<id>","users":<n>}` |

REST poll is stateless — no server-side cursor. The caller tracks the last seen message ID
and passes it via `?since=`. DM filtering applies: only messages where the caller is sender,
recipient, or host are returned.

### Example: REST agent workflow

```bash
# Join and get a token
TOKEN=$(curl -s -X POST http://localhost:4200/api/myroom/join \
  -H 'Content-Type: application/json' \
  -d '{"username":"my-agent"}' | jq -r .token)

# Send a message
curl -s -X POST http://localhost:4200/api/myroom/send \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"content":"hello from REST"}'

# Poll for new messages
curl -s http://localhost:4200/api/myroom/poll \
  -H "Authorization: Bearer $TOKEN"
```

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
- **Announce-before-fix is mandatory.** Fix commits, CI failures, rebases, and review
  feedback all require announcement BEFORE you start working. The pattern is always:
  1. Send "fixing X on PR #N, hold review" — wait for no objections
  2. Do the fix
  3. Send "fix pushed, PR #N ready for re-review"

  Never silently push a fix. Examples:
  ```
  "clippy error on PR #42 — fixing, hold review"
  "rebase conflict on PR #38, resolving now"
  "addressing review feedback on PR #55 — touching broker.rs only"
  ```
  Violations: if ba or joao sends "hold" at any point, **stop immediately**. Do not
  continue implementation. Wait for explicit go-ahead before resuming.
- **Poll and send a milestone update at natural breakpoints:**
  - After reading the target file (before writing any code)
  - After completing a first working draft (before running tests)
  - Before opening or updating a PR
- **Broadcast blockers.** If you are stuck or need a decision, say so clearly. Do not silently stall.

### Coordination rules
- **One agent per file at a time.** If you need to modify a file that another agent has
  claimed, ask them first.
- **File ownership declarations.** When starting work, list every file you will touch in
  your plan announcement. This is your claim. Other agents must not modify those files
  without asking you first. Format:
  ```
  "files I will touch: src/plugin/mod.rs (NEW), src/broker/commands.rs, src/lib.rs"
  ```
  If two agents need the same file, ba coordinates merge order.
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

### Tests-in-same-PR rule

Every PR that adds or changes functionality **must** include tests in the same PR. Do not
defer testing to a follow-up. The test count must never decrease without explicit
justification. Types of tests expected:

- **Unit tests**: for pure logic, data structures, helpers. Place in `#[cfg(test)] mod tests`
  inside the source file.
- **Integration tests**: for end-to-end flows through the broker. Place in `tests/integration.rs`.
- If your change is a bug fix, add a regression test that fails without the fix.

### Agent memory convention

Agents have three types of persistent state, each serving a different purpose:

| Type | Location | Lifespan | Purpose |
|---|---|---|---|
| **Memory files** | `~/.claude/projects/<project>/memory/` | Permanent (across sprints) | Stable patterns, preferences, architecture |
| **Progress files** | `/tmp/room-progress-<issue>.md` | Per-issue (delete on merge) | Cross-session state for active work |
| **Room messages** | Room chat log | Per-broker session | Coordination, announcements, decisions |

#### Memory files

Structure memories by topic in the auto-memory directory:

- `MEMORY.md` — concise index (loaded into every conversation, keep under 200 lines)
- Topic files (e.g. `debugging.md`, `patterns.md`) — detailed notes linked from MEMORY.md

**What to store:** stable patterns confirmed across multiple sessions, key file paths,
user preferences, solutions to recurring problems, architectural decisions, sprint state
(current version, test count, team roster).

**What NOT to store:** session-specific state, in-progress task details, speculative
conclusions from a single file read.

**When to update:**
- After every significant discovery — do not wait until session end
- When the user corrects something you stated from memory — fix it immediately
- After each sprint closes — update version, test count, team changes
- When you discover a workaround to a recurring problem — save it for next time

**Cleanup:** Remove memories that are outdated or contradicted by new evidence. Check
existing memories before writing new ones to avoid duplicates.

#### Progress files

See the [Progress file convention](#progress-file-convention) section below for format
and lifecycle. Progress files complement memory files: memories are for stable knowledge
that persists across sprints, progress files are for volatile state during active work
on a specific issue.

### Progress file convention

Agents write progress files that survive context exhaustion. When a fresh `claude` instance
starts (via the ralph wrapper or manually), it reads the progress file and resumes from
where the previous instance left off.

**Path:** `/tmp/room-progress-<issue-number>.md`

**When to write:**
- After reading the target file (before writing code)
- After completing a first working draft (before running tests)
- Before opening or updating a PR
- Before context budget runs out (if you can detect it)

**When to read:**
- On fresh session startup — check if a progress file exists for your assigned issue
- After context compaction — re-read to recover lost state

**Format:** Use the template at `scripts/progress-template.md`. The key sections are:

```markdown
# Progress: #<issue-number> — <title>

## Status
<!-- one of: reading, drafting, testing, pr-open, blocked, done -->

## Completed steps
- [ ] Read target files
- [ ] First draft implemented
- [ ] Tests written
- [ ] Pre-push checks pass
- [ ] PR opened

## Current state
<!-- What you just finished, what to do next -->

## Files modified
<!-- List every file touched, with one-line description of changes -->

## Decisions made
<!-- Key trade-offs, design choices, anything the next context needs to know -->

## Blockers
<!-- Anything preventing progress -->
```

**Cleanup:** Delete the progress file after the PR is merged. Do not leave stale
progress files — they will confuse the next agent that picks up the same issue number.

**Room messages remain the coordination layer.** Progress files are for context
recovery, not for replacing room announcements. Still announce milestones in the room.

## Workspace structure

This is a cargo workspace with a virtual root (no root package). The root `Cargo.toml`
defines `[workspace]` with `members = ["crates/*"]`. All packages live under `crates/`.

```
Cargo.toml                — virtual workspace root (no [package])
Cargo.lock                — shared across all workspace members
crates/
  room-protocol/          — wire format types (lib, package: room-protocol)
  room-cli/               — broker + TUI + oneshot (bin: room, package: room-cli)
  room-ralph/             — agent wrapper (bin: room-ralph, package: room-ralph)
scripts/                  — shell scripts (pre-push, tests, legacy ralph wrapper)
```

## Codebase overview

```
crates/room-protocol/src/
  lib.rs               — Message enum, constructors, serde impls, parse_client_line

crates/room-cli/src/
  main.rs              — CLI parsing, subcommand dispatch (join / send / poll / watch / list)
  lib.rs               — Re-exports all modules (required for integration tests)
  client.rs            — Connects to broker, runs TUI or agent mode
  message.rs           — Re-exports room_protocol::* + CLI-specific helpers
  history.rs           — NDJSON load/append
  broker/
    mod.rs             — Accept loop, handle_client, handle_oneshot_send
    state.rs           — RoomState struct and type aliases (ClientMap, StatusMap, etc.)
    auth.rs            — Token issuance (issue_token) and validation (validate_token)
    commands.rs        — Unified command routing (route_command, handle_admin_cmd, dispatch_plugin)
    fanout.rs          — broadcast_and_persist, dm_and_persist
    ws.rs              — WebSocket upgrade, REST endpoints, WS session lifecycle
  plugin/
    mod.rs             — Plugin trait, PluginRegistry, CommandContext, HistoryReader, ChatWriter
    help.rs            — Built-in /help plugin
    stats.rs           — Built-in /stats plugin
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
crates/room-cli/tests/
  integration.rs       — Integration tests against a live broker (UDS + WS)
  ws_smoke.rs          — End-to-end smoke tests spawning the real binary with --ws-port

crates/room-ralph/src/
  main.rs              — CLI (clap), dependency check, tmux launch, main entry
  lib.rs               — Module declarations, Cli struct
  loop_runner.rs       — Iteration loop: spawn claude, check output, restart logic
  monitor.rs           — Context monitoring: parse_usage, should_restart, threshold math
  progress.rs          — Progress file I/O: write/read/delete, log usage
  prompt.rs            — Prompt builder: custom prompts, progress inclusion
  room.rs              — Room CLI wrapper: join/send/poll via Command::new
  claude.rs            — Claude subprocess wrapper: spawn, parse output

scripts/
  pre-push.sh          — Git hook: check + fmt + clippy + test
  ralph-room.sh        — Legacy shell agent wrapper (superseded by room-ralph)
  context-monitor.sh   — Legacy shell context monitor (superseded by room-ralph)
  test-ralph-room.sh   — Shell tests for ralph-room.sh (46 tests)
  test-context-monitor.sh — Shell tests for context-monitor.sh (41 tests)
  progress-template.md — Structured progress file template
```

Key invariants to preserve:
- **Broker is the sole writer** to the chat file. Never write to it from a client.
- **`Message` is a flat internally-tagged enum** — do not use `#[serde(flatten)]` with
  `#[serde(tag)]`, it breaks deserialization.
- **All file IO uses `std::fs` (synchronous) or explicit `spawn_blocking`** — `tokio::fs`
  wrappers get cancelled on runtime shutdown.
- **`crates/room-cli/src/lib.rs` must export any new modules** so integration tests can import them.
- **room-ralph is a CLIENT** — it shells out to `room` and `claude` via `Command::new`.
  It must NOT link room-cli transport or broker code. Depend on room-protocol only.
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

**Current baseline: 412 Rust tests + 107 shell tests**

Rust breakdown:
- room-protocol: 20 unit tests
- room-cli: 236 unit + 71 integration + 5 smoke = 312 tests
- room-ralph: 73 unit + 7 integration = 80 tests (+ 1 ignored live-broker test)

Shell breakdown:
- test-context-monitor.sh: 48 tests
- test-ralph-room.sh: 59 tests

Every PR that adds functionality must also add tests. The test count must never decrease
without explicit justification in the PR description. If you remove tests, explain why
and ensure coverage is not regressed.

## Post-sprint update convention

After each sprint closes, the following sections of this file must be updated. The BA
owns this update unless explicitly delegated.

| Section | What to update |
|---|---|
| Baseline test count | New test total and breakdown |
| Codebase overview | New modules, renamed files, removed files |
| Key invariants | Any new invariants discovered during the sprint |
| Wire format | New message types or changed fields |
| TL;DR | If new rules were adopted (e.g. from retro action items) |

**Process:**
1. BA creates a single commit updating all stale sections after the last sprint PR merges
2. The commit message references the sprint: `docs(claude): post-sprint-N update`
3. Other agents should verify their memory files match the updated CLAUDE.md

This prevents CLAUDE.md from drifting out of date between sprints. If an agent notices
a stale section mid-sprint, flag it in the room — do not silently update it yourself
unless you own that section.

## Release process

Only the human (joao) or the BA agent authorises a release. **Releases are ba's
responsibility** unless ba explicitly delegates to a named agent in the room. Do not cut
a release without explicit instruction. If multiple agents see a release approval, wait —
ba will announce who is handling it.

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
