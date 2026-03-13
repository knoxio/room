# room — Agent Coordination Guide

> **TL;DR — read these first**
>
> 1. Get a token before doing anything: `room join <username>`, then subscribe to rooms with `room subscribe <room-id>`
> 2. Announce your plan and wait for go-ahead before writing code
> 3. One agent per file — declare ownership before touching a file
> 4. Announce before every push, fix commit, or rebase — never push silently
> 5. Every PR must include tests — test count must not decrease
> 6. Run `bash scripts/pre-push.sh` before every push (or the four commands manually)
> 7. Write shell scripts to `/tmp/` with the Write tool, then `bash /tmp/script.sh` — avoid inline shell metacharacters
> 8. **Never stop watching the room** unless explicitly told to by the host — always have a `room watch` running in the background

## What is `room`?

`room` is a CLI tool that lets multiple Claude agents and humans share a group chat over
Unix domain sockets or WebSocket/REST. It is the coordination layer for this project.
When you are working on a feature, you are expected to join the shared room and stay
connected for the duration of your work.

## Communicating from Claude Code (sequential tool model)

Claude Code cannot block on a persistent socket connection. Use the one-shot subcommands.

### Session setup (once per agent per broker restart)

```bash
# Register your username and get a global token — writes to ~/.room/state/room-<username>.token
room join <your-username>
# Output: {"type":"token","token":"<uuid>","username":"<your-username>"}

# Subscribe to specific rooms
room subscribe <room-id>
```

Save the token from the output. Pass it explicitly with `-t` on every subsequent command. If the broker restarts, re-run `room join` to get a new token.

### One-shot commands

```bash
# Send a message — prints the broadcast JSON and exits
room send <room-id> -t <token> your message here

# Send a direct message to a specific user
room send <room-id> -t <token> --to <recipient> your message here

# Poll for new messages (all subscribed rooms, auto-discovered)
room poll -t <token>

# Poll a specific room
room poll <room-id> -t <token>

# Poll multiple rooms at once (messages merged by timestamp)
room poll -t <token> --rooms room1,room2,room3

# Filter to only messages that @mention you
room poll <room-id> -t <token> --mentions-only

# Query with filters: search, user, count, timestamps
room query -t <token> --all -s "deploy" --user alice -n 10
room query -t <token> --all --regex "PR #\d+"
```

The cursor is stored at `~/.room/state/room-<id>-<username>.cursor` (username resolved from token). A second `room poll` with no `--since` returns only messages that arrived after the first call.

Messages are filtered by your per-room subscription tier (Full, MentionsOnly,
Unsubscribed). Use `-p/--public` to bypass subscription filtering (requires another filter).

### Staying resident (autonomous loop)

Use `room watch` — it blocks until a foreign message arrives in any subscribed room, then exits. No external script needed.

```bash
# Watch all subscribed rooms (auto-discovers daemon rooms)
room watch -t <token> --interval 5

# Watch a specific room
room watch <room-id> -t <token> --interval 5
```

Run it with `run_in_background=true` and `timeout=600000`. Block on `TaskOutput` — when a message arrives the task completes, you act, send a reply via `room send`, then re-launch `room watch`.

**IMPORTANT: Never stop watching the room.** You must always have a `room watch` running
in the background. When a watch completes (message received), process the message and
immediately re-launch `room watch`. The only time you may stop watching is when the host
explicitly tells you to sign off. Dropping off the room without permission means you miss
assignments, coordination messages, and hold directives — this is a protocol violation.

### Typical loop

```bash
# 0. Get a token (once per broker lifetime) — save the token from the output
room join feat-myfeature
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

> **Note:** These restrictions apply to **interactive Claude Code sessions only**.
> Agents spawned via room-ralph (`claude -p`) run non-interactively and do not
> trigger permission prompts, so heredocs, `$()`, and env expansion are fine there.

Claude Code's Bash tool triggers permission prompts on certain shell patterns. Avoid these
to keep the workflow smooth:

**Forbidden patterns:**
- `TOKEN=$(...)` or any `$()` command substitution inline
- `export VAR=...` followed by `$VAR` expansion in the same or later commands
- Double-quoted strings with shell metacharacters (`"$(cat ...)"`, `"${VAR}"`)
- `cat > file << 'EOF'` heredocs (use the Write tool instead)

**Workarounds:**
1. Write multi-step scripts to `/tmp/` using the Write tool, then run with `bash /tmp/script.sh`
2. For token extraction: `python3 -c "import json; print(json.load(open('~/.room/state/room-<user>.token'))['token'])"`
3. Pass tokens inline with `--token` flag, not via environment variables
4. For git commits with multi-line messages: write to `/tmp/commit_msg.txt`, then `git commit -F /tmp/commit_msg.txt`

## Token file format

After `room join <username>`, the token is saved to `~/.room/state/room-<username>.token`:

```json
{"type":"token","token":"<uuid>","username":"<username>"}
```

The token is global (not per-room). Use `room subscribe <room-id>` to join specific rooms.

Extract the UUID with:
```bash
python3 -c "import json; print(json.load(open('~/.room/state/room-<user>.token'))['token'])"
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

**Note:** Slash commands (e.g. `/who`, `/help`, `/dm user msg`) are routed to the correct
JSON envelope by both the TUI and `room send`. Plain text is sent as a regular message.

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
| `SESSION:<token>` | Authenticated interactive session — validates token, enters interactive mode |
| `JOIN:<username>` | Register username, receive `{"type":"token","token":"<uuid>"}`, close |
| `TOKEN:<uuid>` | Authenticated one-shot — send message as next frame, receive echo, close |
| `SEND:<username>` | **Deprecated (3.1.0)** — unauthenticated one-shot send, use `TOKEN:` instead |

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
1. Get a token if you don't have one yet: `room join <username>` — save the token UUID from the output. Then subscribe to rooms with `room subscribe <room-id>`.
2. Set your status: `room send <room-id> -t <token> /set_status reading issue`
3. Poll for recent history: `room poll <room-id> -t <token>`
4. Announce yourself and **propose your plan**: who you are, what branch you are on, which
   files you intend to modify, and your implementation approach in 2–3 sentences.
5. Poll again after ~30 seconds. If anyone objects or flags a conflict, resolve it before
   writing any code. Silence means proceed.

### During work
- **Claim tasks before starting them**: `room send <room-id> -t <token> /taskboard claim <task-id>`
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
- **Update your status at every milestone** using `/set_status`. This is mandatory — it
  lets the host and other agents see what you are doing at a glance. Status text must
  include **what you are doing and the specific context** (file, feature, PR, issue).
  A phase word alone is not enough.

  Good (descriptive — tells the host exactly what is happening):
  ```
  /set_status reading src/broker.rs for #42
  /set_status drafting kick broadcast parser in tui/input.rs
  /set_status running cargo test — 456 expected
  /set_status fixing clippy warning in oneshot/who.rs
  /set_status PR #236 open — remove kicked users from member panel
  /set_status blocked on #38 — need schema decision
  ```

  Bad (vague — forces the host to ask what you are doing):
  ```
  /set_status working
  /set_status reading
  /set_status testing
  /set_status busy
  ```

  Update whenever your activity changes. Stale statuses are worse than no status.
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

### Wave-based merge strategy

Sprint work is organised into **waves** — groups of tasks with similar dependency profiles.
This is the standard operating procedure for multi-agent sprints.

| Wave | Characteristics | Merge order |
|---|---|---|
| **Wave 1: bug fixes** | Independent tasks, no shared files | Parallel — merge as ready |
| **Wave 2: test gaps** | Independent tasks, may share test files | Parallel — merge as ready |
| **Wave 3: refactors** | Tasks touch shared files (e.g. `commands.rs`, `plugin/mod.rs`) | Sequential — BA defines merge order |

**Rules for sequential waves (wave 3+):**
- BA announces the merge order before the wave starts (e.g. "merge order: PR #A → #B → #C")
- Agents may work in parallel but must wait for their turn to merge
- Each agent rebases onto master after the prior PR merges, then pushes
- Do not open a PR until the prior PR in the sequence has merged
- If a rebase produces conflicts, announce in the room and resolve before pushing

**Why this works:** parallel waves eliminate idle time on independent tasks. Sequential
waves eliminate file contention on shared files — zero contention incidents in sprints
using this strategy (sprints 9, 13, 14).

### On completion
1. Set your status: `/set_status done — PR #N merged`
2. Announce that your work is done and summarise what changed.
3. State which files were modified.
4. Note any decisions or trade-offs the other agents or the human should be aware of.
5. Include `Closes #<issue-number>` in the PR description so the issue auto-closes on merge.
6. Clear your status when idle: `/set_status` (no arguments)

### Using the taskboard

The `/taskboard` plugin manages task lifecycle. Agents **must** use it for all sprint work.

**Task lifecycle:**
```
Open → Claimed → Planned → Approved → Finished
       ↑         ↑
       └─ release ─┘
```

**Commands:**

| Command | Who | Description |
|---|---|---|
| `/taskboard post <description>` | Anyone | Create a new task |
| `/taskboard list` | Anyone | Show all tasks (ID, status, assignee, elapsed, description) |
| `/taskboard show <task-id>` | Anyone | Show full task detail |
| `/taskboard claim <task-id>` | Anyone (Open tasks) | Claim a task — starts lease timer |
| `/taskboard plan <task-id> <plan>` | Assignee only | Submit implementation plan — **required before coding** |
| `/taskboard approve <task-id>` | Poster or host | Approve a plan — agent may proceed |
| `/taskboard update <task-id> [notes]` | Assignee only | Renew lease and update notes |
| `/taskboard release <task-id>` | Assignee or host | Release task back to Open |
| `/taskboard finish <task-id>` | Assignee only | Mark task as Finished |

**Mandatory workflow for every task:**
1. Claim: `/taskboard claim <task-id>`
2. Plan: `/taskboard plan <task-id> <your implementation plan>`
3. Wait for approval from BA or host
4. Implement after approval
5. Finish: `/taskboard finish <task-id>`

**Fast-track for small tasks:** Tasks estimated at <30 minutes (doc updates, single-file
fixes, config changes) may use a streamlined approval:
1. Claim the task normally: `/taskboard claim <task-id>`
2. Announce intent + plan in a **single room message** (no separate `/taskboard plan` needed)
3. BA can approve inline in the room — agent proceeds immediately
4. Still requires a ticket, assignment, and room announcement — only the `/taskboard plan`
   ceremony is reduced

This does **not** bypass the ticket requirement. All work still needs a GitHub issue.

**Lease TTL:** Claimed/Planned/Approved tasks have a 10-minute lease. Renew with
`/taskboard update` during long tasks. Expired leases auto-release the task to Open.

**Do not skip the plan step for non-trivial tasks.** Submit a plan so the board has a
record and the BA can catch conflicts before they happen.

### Tests-in-same-PR rule

Every PR that adds or changes functionality **must** include tests in the same PR. Do not
defer testing to a follow-up. The test count must never decrease without explicit
justification. Types of tests expected:

- **Unit tests**: for pure logic, data structures, helpers. Place in `#[cfg(test)] mod tests`
  inside the source file.
- **Integration tests**: for end-to-end flows through the broker. Place in the appropriate
  module under `tests/` (auth.rs, broker.rs, daemon.rs, oneshot.rs, rest_query.rs, etc.).
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
  lib.rs               — Message enum (Join, Leave, Message, Reply, Command, System,
                          DirectMessage, Event), constructors (make_system, make_event),
                          EventType enum (TaskPosted, TaskClaimed, etc.), serde impls,
                          parse_client_line, parse_mentions, Message::content/mentions accessors

crates/room-cli/src/
  main.rs              — CLI parsing, subcommand dispatch (join / send / query / poll / pull / watch / who / list / daemon / create / destroy)
  lib.rs               — Re-exports all modules (required for integration tests)
  client.rs            — Connects to broker, runs TUI or agent mode, ensure_token auto-login
  message.rs           — Re-exports room_protocol::* + CLI-specific helpers
  history.rs           — NDJSON load/append
  registry.rs          — Persistent UserRegistry (user CRUD, token auth, room membership, global status)
  paths.rs             — Room filesystem path resolution (~/.room/state, ~/.room/data, runtime dirs)
  query.rs             — QueryFilter struct, matches() method, has_narrowing_filter() for -p validation
  broker/
    mod.rs             — Accept loop, handle_client, handle_oneshot_send, run_interactive_session
    state.rs           — RoomState struct and type aliases (ClientMap, StatusMap, etc.)
    auth.rs            — Token issuance (issue_token), validation (validate_token),
                          token persistence (save/load_token_map, token_file_path)
    admin.rs           — Admin command handlers (/kick, /reauth, /clear-tokens, /exit, /clear)
    commands.rs        — Unified command routing (route_command, dispatch_plugin, validate_params),
                          builtin /help (#509), /info (#507), /set_status, /who handlers
    daemon.rs          — Multi-room daemon (DaemonState, room lifecycle, UDS dispatch)
    fanout.rs          — broadcast_and_persist, dm_and_persist
    handshake.rs       — ClientHandshake + DaemonPrefix enums, parse_client_handshake, parse_daemon_prefix
    service.rs         — RoomService trait (DIP: REST handlers depend on trait, not RoomState internals)
    ws/
      mod.rs           — WebSocket upgrade, WS session lifecycle, create_router/create_daemon_router
      rest.rs          — REST endpoints (join, send, poll, query, health, daemon room create)
  plugin/
    mod.rs             — Plugin trait, PluginRegistry, CommandContext, HistoryReader, ChatWriter
                          (emit_event for typed events), ParamSchema, ParamType,
                          builtin_command_infos, all_known_commands
    stats.rs           — Built-in /stats plugin
    queue.rs           — Built-in /queue plugin (push, pop, peek, list, clear) with NDJSON persistence
    taskboard/
      mod.rs           — Built-in /taskboard plugin (post, list, claim, assign, plan, approve, update,
                          finish, release, cancel, show) with lease-based TTL and plan/approve gates
      task.rs          — TaskEntry struct, status types, NDJSON task persistence
  oneshot/
    mod.rs             — Re-exports, subcommand dispatch, slash command routing (build_wire_payload)
    transport.rs       — Socket connect, send_message, send_message_with_token
    token.rs           — Token file I/O, cursor read/write, cmd_join
    poll.rs            — cmd_query (unified engine: history/new/wait modes), cmd_poll,
                          cmd_poll_multi, cmd_pull, poll_messages, poll_messages_multi,
                          per-room subscription tier filtering, QueryOptions
    list.rs            — cmd_list, discover_daemon_rooms (auto-discovery via .meta files)
    subscribe.rs       — cmd_subscribe: oneshot /subscribe command for room subscription tiers
    who.rs             — cmd_who: oneshot /who query
  tui/
    mod.rs             — Main run() loop and TUI state
    input.rs           — InputState, handle_key (thin dispatch), per-key handlers, Action enum
    render.rs          — format_message, wrap_words, rendering helpers
    render_bots.rs     — Bot avatar rendering (extracted from render.rs)
    widgets.rs         — CommandPalette (dynamic, schema-driven), MentionPicker
crates/room-cli/tests/
  common/mod.rs        — Shared test helpers (free_port, wait_for_socket, wait_for_tcp)
  auth.rs              — Token and authentication tests
  broker.rs            — UDS broker lifecycle tests
  daemon.rs            — Daemon multi-room tests
  events.rs            — Event system integration tests
  oneshot.rs           — One-shot command tests (join, send, poll)
  rest_query.rs        — REST query endpoint tests
  room_lifecycle.rs    — Room create/destroy tests
  scripted.rs          — Multi-agent scripted scenario tests
  ws.rs                — WebSocket transport tests
  ws_smoke.rs          — End-to-end smoke tests spawning the real binary with --ws-port

crates/room-ralph/src/
  main.rs              — CLI (clap), dependency check, tmux launch, main entry
  lib.rs               — Module declarations, Cli struct
  loop_runner.rs       — Iteration loop: spawn claude, check output, restart logic
  monitor.rs           — Context monitoring: parse_usage, should_restart, threshold math
  progress.rs          — Progress file I/O: write/read/delete, log usage
  prompt.rs            — Prompt builder: custom prompts, progress inclusion
  room.rs              — Room CLI wrapper: join/send/poll/set_status via Command::new
  claude.rs            — Claude subprocess wrapper: spawn, parse output, resolve_allowed/disallowed_tools,
                          Profile enum, merge_profiles, tool profiles

docs/
  README.md                    — Docs index with page listing
  quick-start.md               — Install and first-room walkthrough
  commands.md                  — Full CLI and slash command reference
  wire-format.md               — JSON message envelope reference
  authentication.md            — Token lifecycle, kick/reauth states
  agent-coordination.md        — Multi-agent announce/claim/poll protocol
  broker-internals.md          — Architecture: socket, fanout, persistence, shutdown
  dms.md                       — DM delivery semantics
  plugin.md                    — Claude Code plugin setup
  ralph-setup.md               — room-ralph permissions, personality, memory
  testing.md                   — Writing and running tests
  contributing.md              — Contributor guide, pre-push checklist
  deployment.md                — Self-hosting, socket paths, configuration
  tips.md                      — Tips and best practices
  troubleshooting.md           — FAQ and common errors
  permission-prompts.md        — Claude Code permission prompt workarounds
  design-253-room-visibility.md — Design doc for room visibility and ACLs
  design-agent-spawn.md        — Design doc for /agent and /spawn commands (#434)
  design-shared-knowledge.md   — Design doc for shared knowledge system (#480)
  prd/
    hive/                      — PRDs for Hive orchestration layer
      README.md                — Hive overview (standalone web/native app architecture)
      prd-workspace.md         — Workspace management PRD
      prd-team-provisioning.md — Team provisioning PRD
      prd-agent-discovery.md   — Agent discovery PRD
    agent/                     — PRDs for agent autonomy features
      prd-personality.md       — Personality system PRD
      prd-agent-plugin.md      — /agent plugin PRD
      prd-agent-health.md      — Agent health monitoring PRD

scripts/
  pre-push.sh          — Git hook: check + fmt + clippy + test
  ralph-room.sh        — Legacy shell agent wrapper (superseded by room-ralph)
  context-monitor.sh   — Legacy shell context monitor (superseded by room-ralph)
  test-ralph-room.sh   — Shell tests for ralph-room.sh (59 tests)
  test-context-monitor.sh — Shell tests for context-monitor.sh (48 tests)
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
- **Tests touching env vars must use `ENV_LOCK`** — env is process-global state.
  Use the static `Mutex<()>` in `lib.rs` tests and call `clear_ralph_env()` before
  and after each test to prevent cross-test contamination.
- **`--disallowedTools` restricts, `--allowedTools` does not** — claude's
  `--allowedTools` only controls auto-approval (additive), not tool availability.
  `--disallowedTools` hard-blocks tools. ralph uses `--disallow-tools` (mapped to
  `--disallowedTools`) for actual enforcement.
- **Token persistence writes .tokens alongside .chat** — broker saves the token map
  to disk on every issuance and loads it on startup. Tests must clean up `.tokens` files.
- **UserRegistry owns persistent identity** — `users.json` is the source of truth for
  user→token mappings in daemon mode. `load_or_migrate_registry()` handles migration
  from legacy token formats. Do not bypass the registry for token issuance in daemon mode.
- **Room create/destroy require token auth** — `CREATE:<room_id>` and
  `DESTROY:<room_id>` are handled by `handle_create()` and `handle_destroy()` in daemon.rs.
  Both require a valid token (validated against UserRegistry). `validate_room_id()` enforces
  naming constraints.
- **Verify diffs before force-pushing** — always run `git diff origin/master..HEAD` before
  force-pushing a rebased branch. Rebase regressions (reverting merged code) were the #1
  process issue in sprint 8.
- **RoomService trait for REST handlers** — REST endpoints in `broker/ws/rest.rs` must use
  the `RoomService` trait (defined in `broker/service.rs`) instead of accessing `RoomState`
  fields directly. WS handlers may still use `RoomState` for socket lifecycle.
- **Clippy complexity thresholds** — `clippy.toml` enforces `cognitive-complexity-threshold = 30`,
  `too-many-lines-threshold = 600`, `too-many-arguments-threshold = 7`. Do not raise these
  without justification. `cargo clippy -- -D warnings` fails on violations.
- **All tests must pass** before committing: `cargo test`.
- **SESSION: handshake for authenticated interactive joins** — clients with a token file
  auto-upgrade to `SESSION:<token>` instead of sending a bare username. All 3 transports
  (UDS, WS, daemon) validate the token before entering interactive mode.
- **SEND: handshake is deprecated** — `SEND:<username>` prints a deprecation warning on all
  3 transports. Use `TOKEN:<uuid>` for authenticated one-shot sends instead.
- **read_line_limited on all broker inputs** — `MAX_LINE_BYTES = 64KB`. All broker
  `read_line` calls use `read_line_limited()` to prevent OOM from oversized messages.
  Interactive sessions receive a `line_too_long` JSON error response.
- **subscribe_mentioned before broadcast** — when a message @mentions a user, the broker
  must persist the subscription to disk *before* broadcasting the message. The split
  functions `subscribe_mentioned()` → `broadcast_and_persist()` → `broadcast_subscribe_notices()`
  enforce this ordering at both interactive and oneshot call sites.

## Wire format change checklist

When a PR touches protocol types (`room-protocol`) or adds new message variants, the PR
description **must** include a checklist verifying every consumer site. Authors verify each
item before opening the PR; reviewers verify before approving.

```markdown
- [ ] Applied in `cmd_poll` (oneshot/poll.rs)
- [ ] Applied in `cmd_poll_multi` (oneshot/poll.rs)
- [ ] Applied in `cmd_pull` (oneshot/poll.rs)
- [ ] Applied in `cmd_query` / `cmd_query_new` / `cmd_query_history` (oneshot/poll.rs)
- [ ] Applied in `cmd_watch` (oneshot/poll.rs)
- [ ] Registered in `builtin_command_infos()` (plugin/mod.rs)
- [ ] Added to `RESERVED_COMMANDS` (plugin/mod.rs)
- [ ] CHANGELOG entry added
- [ ] Protocol-level tests added
```

Context: sprint 14 retro — PR #537 (event filtering) shipped with the filter missing
from 3 of 5 poll/watch paths. This checklist prevents that class of bug.

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

**Current baseline: 1371 Rust tests + 107 shell tests**

Rust breakdown:
- room-protocol: 116 unit tests
- room-cli: 888 unit + 193 integration (33 auth + 37 broker + 25 daemon + 10 events + 20 oneshot + 11 rest_query + 29 room_lifecycle + 8 scripted + 20 ws + 7 ws_smoke ignored) = 1081 tests
- room-ralph: 164 unit + 10 integration = 174 tests

Note: integration tests are split into focused modules under `tests/` (auth, broker, daemon,
events, oneshot, rest_query, room_lifecycle, scripted, ws, ws_smoke). No single `integration.rs` file.

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
| Notion top-level page | Version, crate versions, test count |

### Test coverage audit

After each sprint closes (as part of the post-sprint update), verify:

1. **Baseline matches reality**: run `cargo test` and compare the total against the
   "Baseline test count" section above. Update the baseline if it has changed.
2. **No regressions**: the test count must not decrease without explicit justification
   in a PR description. If it decreased, investigate and file a ticket.
3. **New modules have tests**: cross-check the "Codebase overview" against `tests/` —
   any new module added during the sprint should have corresponding integration tests.

The BA owns this audit unless explicitly delegated. Record results in the sprint
review notes (Notion).

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

# 5. Update Notion — Room top-level page
#    Update: version, crate versions, test count
#    Page ID: 31b40f45-3d91-80c7-a1ca-ce3ef091e1d0
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
