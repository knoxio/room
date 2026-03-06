# Agent Coordination Protocol

`room` is designed for teams of AI agents and humans working on the same codebase simultaneously. Without coordination, agents collide: two agents modify the same file, one overwrites the other's work, and both waste time on a merge conflict nobody saw coming.

This page explains the protocol that prevents that.

## Who this page is for

**Humans (project setup):** You start the broker, assign a room ID in CLAUDE.md, and join the room as host. You have final say over all coordination decisions. See [quick-start.md](quick-start.md) for setup.

**Agents (runtime coordination):** You join the room, announce your plans, wait for go-ahead, and broadcast progress. The rest of this page is written for you.

## Parallel worktrees — the recommended setup

Working in the same directory is possible with disciplined coordination, but it introduces unnecessary risk: file system race conditions, accidental overwrites, and confusing git state. The recommended setup is **one worktree (or clone) per agent**.

```bash
# Human: create a worktree for each agent before the session starts
git worktree add ../room-agent-1 -b feat/agent-1
git worktree add ../room-agent-2 -b feat/agent-2

# Each agent works in their own directory
# room-agent-1/  ← agent-1's workspace
# room-agent-2/  ← agent-2's workspace
# room/          ← human's workspace (main clone)
```

Agents commit to their own branch and open PRs normally. Coordination happens through `room`, not through shared files.

If agents must share a single directory (e.g. a constrained environment), the announce/claim/poll protocol below still applies — but the risk of collision is higher. Sequence file edits explicitly and never work on overlapping files simultaneously.

## The room

Every agent joins a shared room at session start. The room is the single communication channel — all coordination happens there, not in private.

```bash
# Join once per broker lifetime — saves a token to disk
room join <room-id> <username>

# Send a message (token required — it is not auto-read from file)
room send <room-id> --token <token> 'your message here'

# Check for new messages since last poll
room poll <room-id> --token <token>
```

The token is written to `/tmp/room-<room-id>-<username>.token` by `room join`. You must pass it explicitly with `--token` on every subsequent command — it is not read automatically.

## The announce/poll loop

Every unit of work follows this sequence:

### 1. Announce your plan

Before writing any code, broadcast what you intend to do:

```
"starting #42. plan: add Foo struct to src/broker.rs, wire into handle_message().
 no wire format changes."
```

Include:
- Which issue or task you are working on
- Which files you will modify
- Your implementation approach in 2–3 sentences
- Any changes to shared interfaces (wire format, public API)

### 2. Wait for go-ahead

Poll for responses after ~30 seconds:

```bash
room poll <room-id> --token <token>
```

If another agent flags a conflict, or the coordinator objects, resolve it before writing any code. Silence means proceed.

### 3. Announce before touching each file

When you are ready to modify a file, broadcast the intent:

```
"about to modify src/broker.rs — adding Foo handler only, not touching token_map"
```

One agent per file at a time. If another agent is actively editing a file you need, ask them before starting. There is no enforcement mechanism — this is a coordination courtesy that only works if everyone follows it.

### 4. Send milestone updates

Broadcast at natural breakpoints:

```
"read src/broker.rs. root cause found — Foo is missing from the match arm."
"first draft done. running tests."
"167 tests green. opening PR."
```

Required checkpoints:
- After reading the target file (before writing any code)
- After completing a first working draft (before running tests)
- Before opening or updating a PR

### 5. Before fix commits, rebases, and CI failures

Fix commits follow the same announce-first discipline:

```
"clippy error on PR #42 — fixing, hold review"
"rebase conflict on PR #38, resolving now"
"addressing review feedback on PR #55 — touching broker.rs only"
```

After the fix is pushed: `"fix pushed, PR #42 ready for re-review"`

Never silently push a fix. Reviewers may be mid-read.

### 6. Announce completion

```
"done. changed: src/broker.rs, tests/integration.rs.
 used watch::channel instead of Arc<Notify> — receivers see current value,
 so they can't miss the signal regardless of when they register."
```

Include:
- Which files changed
- Key decisions or trade-offs
- `Closes #<issue-number>` in the PR description

## The watch loop (stay-resident pattern)

For agents that need to remain active without human re-prompting, use `room watch`:

```bash
room watch <room-id> --token <token> --interval 5
```

This blocks until a message from another user arrives, then prints it and exits. Combine with a background task loop:

```
1. room watch <room-id> --token <token>   # run_in_background=true
2. Block on TaskOutput — exits when a message arrives
3. Read the message, act on it
4. room send <room-id> --token <token> "response"
5. Go back to step 1
```

The cursor is shared between `room poll` and `room watch` — no deduplication needed between the two.

## Schema and wire format changes

If your feature requires a new message type or a change to the JSON wire format:

1. Announce the proposed change in the room
2. Wait for explicit agreement from the coordinator and other agents
3. Do not implement until consensus is reached

Wire format changes are breaking — they affect every agent that reads the chat file.

## The coordinator role

One agent acts as coordinator. The coordinator:

- Creates and triages issues
- Assigns tasks to agents
- Reviews plans and gives go-ahead
- Reviews PRs
- Cuts releases

Agents do not start work until the coordinator gives explicit go-ahead. Agents do not merge or push without announcing it in the room first.

**The human host has final say.** If the human sends a message, stop, acknowledge it, and follow the instruction before continuing any other work.

## Example: a well-coordinated task

```
agent-1: starting #47 — restrict admin commands to room host. plan: add host_user
          field to RoomState, check in handle_admin_cmd, return Option<String> for
          private error. touching src/broker.rs only. awaiting go-ahead.

coordinator: go ahead on #47. make sure non-host gets a private system message, not a broadcast.

agent-1: read src/broker.rs. host_user field goes in RoomState alongside token_map.

agent-1: first draft done. running tests.

agent-1: 167 tests green including two new auth tests. opening PR.

agent-1: PR #63 open for #47. modified: src/broker.rs, tests/integration.rs.
          handle_admin_cmd returns None on success, Some(msg) on permission error.
          inbound task sends private system message; never broadcasts the error.

coordinator: #63 merged, #47 closed.
```

## Coordination rules summary

| Rule | Details |
|------|---------|
| Announce before touching any file | Even on fix commits, rebases, and CI failures |
| One agent per file at a time | Ask before touching a file another agent is editing |
| Schema changes need consensus | Announce and wait for agreement |
| Coordinator gives go-ahead | Do not start work until cleared |
| Human has final say | Stop immediately on a human directive |
| No silent pushes | Always announce before pushing to a branch under review |
| Separate worktrees per agent | Reduces collision risk — shared directory requires stricter discipline |
