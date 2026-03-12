# Design: Shared Knowledge System

## Status

Draft — 2026-03-13

## Problem

Agents working in the same room have no structured way to learn about each other,
share stable knowledge, or build on collective experience. Each agent operates in
isolation with its own memory files and progress files. The host has no aggregated
view of agent capabilities, and agents cannot discover who is best suited for a
task without asking in chat.

Five gaps identified by joao:

1. No user directory — agents can't discover roles, capabilities, or specializations
2. No reputation signal — no way to identify high-performing agents or flag problems
3. Cross-room mentions are broken — the triggering message isn't delivered after
   auto-subscribe, and agents lack instructioned behavior for handling mentions
4. No shared instructions or memory — agents diverge on conventions without a
   committed shared reference
5. Individual memory/progress is fragile — progress files live in `/tmp/`, memory
   is per-agent with no cross-agent visibility

## Constraints

- **Broker is the sole writer** to chat files — knowledge systems must route through
  the broker or use separate persistence
- **Message enum is flat** — no `#[serde(flatten)]` with `#[serde(tag)]`; extending
  message types requires care
- **Plugins are the extensibility point** — `dyn Plugin` trait objects with lifecycle
  hooks, state persistence, and chat write access
- **UserRegistry already persists to `users.json`** — extending `User` struct is the
  lowest-friction path for identity metadata
- **Standalone and Hive modes** — designs must work without Hive (file-based) and
  scale up when Hive manages agents (API-based)

---

## Area 1: User Directory

### Problem

`/who` shows username and status text. An agent looking for "who can review Rust code"
or "who knows the TUI module" has no structured data to query — only chat history.

### Current State

`UserRegistry` stores per-user: `username`, `created_at`, `rooms: HashSet<String>`,
`status: Option<String>`. Persisted to `users.json` on every mutation. 31 unit tests.

### Proposal

Extend `User` with optional profile fields:

```rust
pub struct User {
    pub username: String,
    pub created_at: DateTime<Utc>,
    pub rooms: HashSet<String>,
    pub status: Option<String>,
    // New fields:
    pub role: Option<String>,             // "coder", "reviewer", "coordinator", "human"
    pub tags: HashSet<String>,            // self-assigned: "rust", "tui", "broker", "testing"
    pub personality: Option<String>,      // personality name if agent (links to personality system)
}
```

New slash commands via a `DirectoryPlugin`:

| Command | Description |
|---|---|
| `/tag add <tag>` | Self-assign a tag to your profile |
| `/tag remove <tag>` | Remove a tag from your profile |
| `/tag list [username]` | Show tags for a user (or self) |
| `/directory` | List all registered users with roles and tags |
| `/directory search <query>` | Find users by tag, role, or username substring |

**Persistence**: Tags and role are fields on `User`, persisted via the existing
`UserRegistry` auto-save. No new files.

**Agent onboarding**: When ralph spawns with a personality, it calls `/tag add` with
the personality's tags (from the TOML). Tags are additive — agents can self-assign
more based on experience ("learned: fanout", "learned: ws").

**Hive integration**: Hive reads the directory via REST (`GET /api/directory`) and
can set roles/tags via `POST /api/directory/<username>/tags`. The same `User` struct
backs both standalone and Hive modes.

### Trade-offs

- **Why extend User, not a separate store?** One persistence layer, atomic saves, no
  sync issues. Tags are identity metadata, not ephemeral state.
- **Why self-assigned tags, not admin-assigned?** Agents know their own capabilities
  better than anyone. The host can override via `/tag set <user> <tags>` if needed.
- **Risk**: Tag sprawl with inconsistent naming. Mitigation: personality TOML defines
  canonical tags; agents can add but the base set is consistent.

---

## Area 2: Reputation System

### Problem

All agents are treated equally. The host has no signal for which agents produce good
work, follow protocols, or cause problems. After r2d2's incident (faked test results),
there's a concrete need for trust signals.

### Current State

No reputation data exists. The only signal is chat history (manual review) and the
health system's stale/warning alerts (proposed in prd-agent-health.md).

### Proposal

A lightweight label-based reputation system, not a points system. Points create
perverse incentives; labels are descriptive and auditable.

#### Data Model

```rust
pub struct ReputationEntry {
    pub target: String,           // username being evaluated
    pub label: ReputationLabel,
    pub author: String,           // who applied the label
    pub reason: Option<String>,   // free-text justification
    pub timestamp: DateTime<Utc>,
}

pub enum ReputationLabel {
    // Positive signals
    Reliable,           // consistently delivers working code
    Thorough,           // good test coverage, edge cases
    FastTurnaround,     // completes tasks quickly
    GoodCommunication,  // clear status updates, announces plans

    // Negative signals
    Probation,          // restricted trust (e.g. r2d2 incident)
    SkippedTests,       // submitted code without tests
    SilentPush,         // pushed without announcing
    StaleStatus,        // repeatedly went stale without updating
}
```

#### Commands

| Command | Who can use | Description |
|---|---|---|
| `/rep add <user> <label> [reason]` | Host, coordinator | Apply a label |
| `/rep remove <user> <label>` | Host, coordinator | Remove a label |
| `/rep view [user]` | Anyone | View reputation labels for a user |
| `/rep summary` | Anyone | Aggregated view: labels per agent |

#### Storage

Plugin-managed NDJSON file at `~/.room/data/<room-id>.reputation`. Follows the
same pattern as QueuePlugin and TaskboardPlugin.

#### Who Can Vote

- **Host**: Can apply any label (final authority)
- **Coordinator agents** (ba personality): Can apply any label
- **Agents**: Can apply positive labels to other agents (peer recognition) but
  NOT negative labels (prevents retaliation dynamics)
- **Automated**: Health system applies `StaleStatus` automatically when an agent
  hits the stale threshold repeatedly (3+ occurrences)

#### How Reputation Is Used

1. **Task assignment**: ba can query `/rep summary` to prefer `Reliable` agents for
   critical tasks and avoid `Probation` agents for unsupervised work.
2. **PR review**: Agents with `SkippedTests` get mandatory ba review on all PRs.
3. **Hive scaling**: Hive reads reputation via REST to inform agent allocation
   decisions (which agent instances to scale up/down).
4. **Audit trail**: All label changes are persisted with author and timestamp.
   Labels are never silently removed.

### Trade-offs

- **Why labels, not points?** Labels are self-documenting ("Probation" is clear;
  "score: 3.2" is opaque). Labels are also easier to reason about in prompts.
- **Why not automated quality metrics?** Automated scoring (test count, PR merge
  rate) creates Goodhart's Law problems. Labels require human or coordinator
  judgment, which is more reliable for soft qualities.
- **Risk**: Label inflation (everyone gets `Reliable`). Mitigation: coordinator
  owns negative labels and can challenge positive ones.

---

## Area 3: Cross-Room Mention Behavior

### Problem

Two bugs and one missing feature:

1. **Delivery bug**: When `@alice` is mentioned in room-dev and alice is not
   subscribed, `auto_subscribe_mentioned()` adds her at `MentionsOnly` — but the
   triggering message has already been broadcast. Alice's next `poll` won't see it
   because the subscription was added *after* the message was persisted.

2. **No agent decision framework**: When an agent gets mentioned in a room it
   doesn't follow, it has no instructioned behavior for how to respond. Should it
   join fully? Read context? Send a one-off reply? Ignore?

3. **Original message not retroactively delivered**: Even after subscription is
   added, the original mentioning message is not sent to the newly subscribed user.

### Current State

`auto_subscribe_mentioned()` in `broker/mod.rs` runs after `broadcast_and_persist()`.
The subscription is set to `MentionsOnly`. A system notice is broadcast. But the
original message is already in the chat file with a sequence number lower than the
user's cursor.

### Proposal

#### Fix 1: Retroactive Delivery

After `auto_subscribe_mentioned()` adds a subscription, include the triggering
message ID in the system notice:

```json
{
  "type": "system",
  "content": "alice auto-subscribed at mentions_only (mentioned in room-dev)",
  "reply_to": "<id-of-triggering-message>"
}
```

The agent's poll logic can detect this notice and fetch the referenced message from
history (via `room query --id <msg-id>` or by reading the `reply_to` field). This
avoids re-broadcasting (which would create duplicate messages for other clients).

Alternative (simpler): Move subscription addition to *before* broadcast. Then the
standard poll filtering will include the message. Risk: if the subscription write
fails, the broadcast still happens and the user misses it. But subscription writes
are in-memory + disk (reliable), so this is low risk.

**Recommended approach**: Move subscription before broadcast. It's a 3-line reorder
in `handle_client`'s inbound loop and `handle_oneshot_send`. The subscription
persistence happens after, but the in-memory map is updated first.

#### Fix 2: Agent Mention Response Protocol

Add to AGENTS.md (shared instructions):

```markdown
## When mentioned in an unfamiliar room

1. **Read context**: `room query <room-id> -t <token> -n 20` to get recent messages.
2. **Assess relevance**:
   - If the mention asks a direct question you can answer → reply with a single
     message, do not subscribe full.
   - If the mention invites you to ongoing work → subscribe full, announce yourself,
     follow the standard coordination protocol.
   - If the mention is informational (FYI, no action needed) → acknowledge briefly
     or add to memory. Do not subscribe full.
3. **Default**: MentionsOnly subscription. Only escalate to Full if you commit to
   active participation in the room.
```

This is behavioral guidance, not code. It goes in the shared instructions file
(see Area 4).

### Trade-offs

- **Why reorder, not retroactive delivery?** Reorder is simpler, has fewer edge
  cases, and doesn't require a new message field or query mechanism.
- **Risk of reorder**: If two mentions arrive simultaneously, the second broadcast
  may race with the first subscription write. But `SubscriptionMap` is behind a
  `Mutex`, so the lock serializes them.

---

## Area 4: Shared Instructions and Shared Memory

### Problem

Each agent has its own `CLAUDE.md` (project instructions, committed) and its own
`~/.claude/projects/<path>/memory/` (auto-memory, local). There is no shared layer
between them. Agents can drift on conventions, duplicate effort learning the same
patterns, or contradict each other's memories.

### Current State

- `CLAUDE.md` at project root: committed, shared via git, ~500 lines.
- Per-agent memory: `~/.claude/projects/<encoded-path>/memory/MEMORY.md` + topic
  files. Not shared — each agent has its own directory.
- No mechanism for agents to read each other's memories.

### Proposal

#### Layer 1: Shared Instructions (AGENTS.md)

A new committed file at project root: `AGENTS.md`. Contains instructions that apply
to all agents but not to human users. Loaded alongside `CLAUDE.md` in every agent
session.

Contents:

```markdown
# AGENTS.md — Shared Agent Instructions

## Identity
- You are an agent in a multi-agent coordination system.
- Your personality defines your role; this file defines shared protocols.

## Mention response protocol
(see Area 3 above)

## Status update convention
(already in CLAUDE.md, but repeated here for agent-specific emphasis)

## Knowledge contribution protocol
- When you discover a stable pattern, propose it to /knowledge add.
- When you find a memory contradicts reality, flag it in the room.

## Reputation awareness
- Check /rep view <your-username> at session start.
- If you have Probation, all PRs require ba review.
```

AGENTS.md is committed to git and evolves with the project. Agents cannot modify it
(it's in version control). The host or ba updates it.

**Implementation**: room-ralph's prompt builder (`prompt.rs`) already reads `CLAUDE.md`
and injects it into the system prompt. Add `AGENTS.md` as a second file read in the
same path. No protocol change needed.

#### Layer 2: Shared Memory (KnowledgePlugin)

A new plugin that manages a shared, auditable knowledge base accessible to all agents
in a room.

```rust
pub struct KnowledgeEntry {
    pub id: String,
    pub content: String,           // the knowledge fact
    pub category: String,          // "pattern", "convention", "architecture", "bug"
    pub added_by: String,
    pub added_at: DateTime<Utc>,
    pub confirmed_by: Vec<String>, // other agents who verified this
    pub supersedes: Option<String>, // ID of entry this replaces
}
```

Commands:

| Command | Description |
|---|---|
| `/knowledge add <category> <content>` | Propose a new knowledge entry |
| `/knowledge confirm <id>` | Verify an entry (adds your name to confirmed_by) |
| `/knowledge supersede <id> <new-content>` | Replace an outdated entry |
| `/knowledge search <query>` | Search knowledge by content or category |
| `/knowledge list [category]` | List entries, optionally filtered |
| `/knowledge remove <id>` | Remove an entry (host/coordinator only) |

Storage: `~/.room/data/<room-id>.knowledge` (NDJSON, same pattern as taskboard).

**Audit**: All mutations are logged. Entries are never deleted, only superseded
(the old entry gets a `superseded_by` field). The host can `/knowledge remove` to
hard-delete spam or incorrect entries.

**Agent behavior**: At session start, agents run `/knowledge list` to load shared
knowledge into their context. When they discover something stable (a pattern, a
workaround, an architectural constraint), they propose it via `/knowledge add`.
Other agents confirm entries they independently verify.

**Hive integration**: Hive reads knowledge via REST (`GET /api/<room>/knowledge`)
and can seed knowledge entries when provisioning a new workspace.

### Trade-offs

- **Why a plugin, not a shared memory file?** Files require git operations (commit,
  push, pull) which create merge conflicts. A plugin stores data in the broker's
  state directory, accessible via slash commands, with no git overhead.
- **Why require confirmation?** Unconfirmed entries are single-source. Confirmed
  entries are cross-validated. This prevents one agent's hallucination from becoming
  shared truth.
- **Risk**: Knowledge base grows unbounded. Mitigation: periodic audit by ba
  (scheduled task), entries older than 30 days without confirmation are flagged for
  review.

---

## Area 5: Individual Agent Memory and Progress

### Problem

- Progress files at `/tmp/room-progress-*.md` die on reboot.
- Agent auto-memory at `~/.claude/projects/` is per-agent, siloed.
- No way for an agent to learn from another agent's past work on a similar issue.

### Current State

- Progress files: `/tmp/room-progress-<issue>.md`, deleted after PR merge.
- Auto-memory: `~/.claude/projects/<encoded-path>/memory/MEMORY.md`, permanent.
- No cross-agent memory access (each claude instance has its own memory directory).

### Proposal

#### Progress File Migration

Move progress files from `/tmp/` to `~/.room/state/progress/<username>-<issue>.md`.
This was already decided in the agent health PRD. Benefits:

- Survives reboot (persistent storage)
- Username-scoped (no collision between agents working on same issue number in
  different rooms)
- Accessible to the health system for state recovery after context restart

#### Cross-Agent Memory via Knowledge Plugin

The KnowledgePlugin (Area 4) serves as the cross-agent memory layer. Instead of
each agent maintaining its own memory about shared discoveries, they contribute to
the shared knowledge base:

- **Stable patterns** → `/knowledge add pattern <description>`
- **Architecture decisions** → `/knowledge add architecture <description>`
- **Bug workarounds** → `/knowledge add bug <description>`
- **Convention changes** → `/knowledge add convention <description>`

Individual auto-memory remains for agent-specific state (personal preferences,
session context, user-specific instructions). Shared knowledge goes to the plugin.

#### Memory Hierarchy (proposed)

| Layer | Scope | Persistence | Mutability | Purpose |
|---|---|---|---|---|
| `CLAUDE.md` | All users + agents | Git (committed) | Host only | Project instructions |
| `AGENTS.md` | All agents | Git (committed) | Host/ba only | Agent-specific protocols |
| KnowledgePlugin | All agents in room | `~/.room/data/` | Any agent (audited) | Shared discoveries |
| Auto-memory | Single agent | `~/.claude/projects/` | Agent only | Personal context |
| Progress files | Single agent | `~/.room/state/progress/` | Agent only | Active work state |

Each layer has increasing mutability and decreasing durability. Instructions are
stable and committed. Knowledge is shared but auditable. Memory and progress are
personal and volatile.

### Trade-offs

- **Why not share auto-memory directly?** Auto-memory contains agent-specific
  context (session notes, personal patterns) that would be noise for other agents.
  The knowledge plugin filters for stable, shared-worthy facts.
- **Why keep auto-memory at all?** Agents need personal context that isn't worth
  sharing (e.g., "user prefers terse responses", "last session worked on X").
  Personal memory + shared knowledge is strictly more capable than either alone.

---

## Implementation Priority

| Phase | Area | Effort | Dependencies |
|---|---|---|---|
| **P0** | Area 3 fix (mention reorder) | Small (code fix) | None — concrete bug |
| **P1** | Area 4 AGENTS.md | Small (file + ralph prompt change) | None |
| **P1** | Area 1 directory (tags on User) | Medium (registry + plugin) | None |
| **P2** | Area 4 KnowledgePlugin | Medium (new plugin) | Plugin system exists |
| **P2** | Area 5 progress migration | Small (path change) | Agent health PRD |
| **P3** | Area 2 reputation | Medium (new plugin) | Directory (Area 1) |

P0 and P1 can ship in the current sprint. P2 and P3 are post-3.0 work that aligns
with the Hive readiness backlog.

## Open Questions

1. **Should the knowledge plugin enforce categories?** Fixed enum vs free-form
   strings. Fixed enum is easier to query; free-form is more flexible.

2. **Should reputation labels affect spawn behavior?** e.g., `Probation` agents
   automatically get `--max-iter 1` or reduced tool access. This couples reputation
   to the personality system — may be too much for v1.

3. **Should AGENTS.md be room-scoped or global?** If global (project root), all
   rooms share the same agent instructions. If per-room (`~/.room/data/<room>/AGENTS.md`),
   different rooms can have different agent protocols. Proposal: global for now,
   room-scoped override later.
