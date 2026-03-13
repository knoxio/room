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

---

## Area 6: OpenViking Analysis

### Background

[OpenViking](https://github.com/volcengine/OpenViking) is an open-source context database
for AI agents, developed by ByteDance's Volcano Engine team (released January 2026,
Apache 2.0). It organizes all agent context — memory, resources, and skills — under a
virtual filesystem with a `viking://` URI scheme.

Investigated for room-ralph integration as part of #523 (r2d2) and #524 (bumblebee).

### OpenViking Architecture

**Three context types under a unified filesystem:**

| Type | URI Prefix | Purpose | Examples |
|------|-----------|---------|---------|
| Resource | `viking://resources/` | External knowledge: docs, code, web pages | `viking://resources/src/broker.rs` |
| Memory | `viking://user/`, `viking://agent/` | Learned patterns, preferences, decisions | `viking://agent/memories/patterns/serde-bug` |
| Skill | `viking://agent/skills/` | Agent instructions, tool definitions, templates | `viking://agent/skills/coordination-protocol` |

**L0/L1/L2 tiered context loading — the core innovation:**

| Layer | Token Budget | Content | When Loaded |
|-------|-------------|---------|-------------|
| L0 (Abstract) | ~100 tokens | One-sentence summary | Always — vector search filtering |
| L1 (Overview) | ~2,000 tokens | Core info, usage scenarios, key decisions | Planning and decision-making |
| L2 (Detail) | Unlimited | Full original content | Deep reading when specifically needed |

An agent scans L0 abstracts across many resources, loads L1 overviews for candidates,
and reads L2 details only for what's relevant. This progressive loading prevents context
window exhaustion proactively — before tokens are consumed, not after.

**Self-evolving memory via `MemoryExtractor`:**

At session end, an LLM-based extractor analyzes task results and feedback, then
auto-updates eight memory categories:

- **User scope**: Profile, Preferences, Entities, Events
- **Agent scope**: Cases, Patterns, Tools, Skills

A `MemoryDeduplicator` prevents duplicate entries. Extracted memories are stored in the
appropriate `viking://user/` or `viking://agent/` directories.

**Directory recursive retrieval:**

1. Analyze query intent → generate multiple typed queries
2. Global vector search on L0 abstracts → identify high-scoring directories
3. Recursive exploration within promising directories
4. Score propagation: `Final = 0.5 × ParentDirScore + 0.5 × VectorSimilarity`
5. Rerank on L1 overviews → load L2 only for final selections

### Mapping to room-ralph

| room-ralph module | What it does | OpenViking equivalent | Overlap |
|---|---|---|---|
| `monitor.rs` | Token usage tracking, threshold-based restart | L0/L1/L2 prevents exhaustion proactively | Complementary — OV optimizes input, ralph monitors output |
| `progress.rs` | `/tmp/` markdown files, last-50-lines dump | `SessionService` + `MemoryExtractor` | Partial — OV is more structured and searchable |
| `prompt.rs` | Static layering: personality → context → progress → messages | Dynamic retrieval via `viking://` URIs | Significant — OV replaces static assembly with relevance-ranked injection |
| `loop_runner.rs` | Subprocess lifecycle, restart on context exhaustion | Nothing — OV is a data layer | None — orthogonal concerns |
| `room.rs` | Room CLI wrapper (join/send/poll) | Nothing — OV has no messaging | None — orthogonal concerns |

### Key Gaps in room-ralph (relative to OpenViking)

1. **No tiered loading**: Progress files are all-or-nothing (full markdown or nothing).
   No ability to load a summary first and drill down only if relevant.
2. **No semantic search**: Progress and knowledge are retrieved by filename, not content
   relevance. An agent can't ask "what do we know about serde bugs?" and get ranked results.
3. **No automatic memory extraction**: Progress files capture raw last-50-lines output.
   No structured extraction of learnings, patterns, or decisions from session history.
4. **No cross-agent memory**: Each agent's progress file is isolated. Agent A can't learn
   from Agent B's experience with a similar issue.
5. **No context compression**: When context exceeds threshold, ralph restarts with a fresh
   prompt. No intermediate summarization or selective eviction.

### Why Not Adopt OpenViking Directly

1. **Wrong complexity ratio**: room-ralph's context management is ~500 lines across 4 Rust
   files. OpenViking is a distributed system with vector databases, embedding pipelines,
   tree builders, and an HTTP server. The operational overhead is not justified at our scale.
2. **Python runtime dependency**: OpenViking is Python-first. The Rust CLI is a thin HTTP
   client. Adopting it would add a Python runtime dependency to a pure Rust binary ecosystem.
3. **OpenClaw-centric**: Primary integration target is ByteDance's OpenClaw coding agent.
   No first-class Anthropic API support — adapter work needed for Claude-based embeddings
   and summarization.
4. **Early stage**: v0.2.3, limited adoption outside ByteDance's ecosystem.
5. **No multi-agent coordination**: OpenViking manages context for individual agents but has
   no messaging, coordination, or task assignment — the problems room already solves.

### What to Adopt from OpenViking

Three concepts are directly applicable to room without the full OV stack:

1. **Tiered context loading (L0/L1/L2)** → see Area 7 below
2. **Structured memory categories** → refine KnowledgePlugin (Area 4)
3. **Automatic memory extraction** → new capability for room-ralph

---

## Area 7: Agent Memory Architecture (OpenViking-Informed)

### Problem

Areas 4 and 5 proposed a KnowledgePlugin and progress file migration. This area extends
those proposals with concrete architecture informed by OpenViking's tiered loading,
memory categorization, and self-evolution patterns — implemented natively in Rust, without
the OpenViking dependency.

### 7.1 Tiered Progress Files (L0/L1/L2 for room-ralph)

**Current state**: `progress.rs` writes raw last-50-lines to `/tmp/room-progress-*.md`.
On context restart, `prompt.rs` injects the entire file into the next prompt. This wastes
tokens on irrelevant output and provides no structure for selective loading.

**Proposed**: Three-tier progress files inspired by OpenViking's L0/L1/L2 model.

```
~/.room/state/progress/<username>-<issue>/
  L0.txt      # ~100 tokens: one-line summary of current state
  L1.md       # ~2,000 tokens: structured status, decisions, blockers
  L2.md       # Unlimited: full session output (current progress file format)
```

**Generation**: When `write_progress()` is called (context exhaustion or milestone):

1. Write L2 (full output, as today — backward-compatible)
2. Generate L1 by extracting structured sections from the session:
   - Current status (one line)
   - Files modified (list)
   - Decisions made (bullet points)
   - Blockers (if any)
   - Next steps (what the next iteration should do)
3. Generate L0 by summarizing L1 to a single line:
   `"implementing #42: broker auth middleware, 3/5 files done, blocked on schema decision"`

**L1 generation strategy**: Two approaches, increasing in sophistication:

- **V1 (template extraction)**: Parse the existing structured progress file format.
  The current `write_progress()` already writes `## Metadata`, `## Last output`,
  `## Status` sections. Extract and compress these into L1 format. No LLM needed.
- **V2 (LLM summarization)**: After claude exits, use a fast model (haiku) to summarize
  the full session output into L1 and L0. Costs ~$0.001 per context cycle. Add as an
  optional `--summarize-progress` flag on room-ralph.

**Prompt assembly changes** (in `prompt.rs`):

```
// Current: inject entire progress file
// Proposed: inject L0 + L1, reference L2 path for deep reading

"--- PROGRESS SUMMARY (L0) ---"
<L0.txt content>
"--- PROGRESS DETAIL (L1) ---"
<L1.md content>
"--- Full session log available at: <L2.md path> ---"
```

This reduces progress context from unbounded to ~2,100 tokens maximum. The agent can
read L2 via the Read tool if it needs the full session output.

**Compatibility**: L2.md is the current progress file format. Agents or tools that
read progress files directly continue to work. L0 and L1 are additive.

### 7.2 Structured Knowledge Categories (OpenViking-Informed)

Area 4 proposed a KnowledgePlugin with free-form `category: String`. OpenViking uses
eight structured categories (Profile, Preferences, Entities, Events, Cases, Patterns,
Tools, Skills). Room's categories should reflect multi-agent coordination, not single-agent
memory:

**Proposed categories for KnowledgePlugin:**

| Category | Purpose | Example |
|----------|---------|---------|
| `pattern` | Recurring code patterns or conventions | "use OnceLock for post-construction setup on shared state" |
| `architecture` | System design decisions and constraints | "broker is sole writer to chat file — never write from client" |
| `bug` | Known bugs, workarounds, and gotchas | "stale cargo cache: run cargo clean -p room-cli after rebase" |
| `convention` | Process or coordination conventions | "announce before every push, even fix commits" |
| `integration` | Cross-crate or cross-system integration notes | "room-ralph shells out to room binary, never links room-cli" |
| `performance` | Performance characteristics and thresholds | "daemon tests need 200ms delay for socket ready" |

**Fixed enum, not free-form** (answering Open Question 1): A fixed enum prevents
category sprawl, enables efficient filtering, and makes knowledge entries predictable
for agents loading them into context. New categories can be added via code change
when the need is demonstrated.

```rust
pub enum KnowledgeCategory {
    Pattern,
    Architecture,
    Bug,
    Convention,
    Integration,
    Performance,
}
```

**Relation to OpenViking's categories**: OpenViking's user-scope categories (Profile,
Preferences, Entities, Events) map to individual agent auto-memory — they're personal.
OpenViking's agent-scope categories (Cases, Patterns, Tools, Skills) map to shared
knowledge. Room's categories are all shared-scope because individual memory is handled
by Claude Code's built-in auto-memory system.

### 7.3 Automatic Memory Extraction (Session-End Processing)

**Current state**: When a ralph iteration ends, `process_output()` logs token usage and
optionally writes a progress file. No structured learnings are extracted.

**Proposed**: After each ralph iteration (or context cycle), extract knowledge entries
from the session output and propose them to the KnowledgePlugin.

**Extraction pipeline** (in room-ralph, not the broker):

1. After claude exits, read the session output (already available in `ClaudeOutput.raw_json`)
2. If the session produced meaningful work (not just polling), run extraction:
   - **V1 (pattern matching)**: Scan output for common knowledge indicators:
     - `"I discovered that..."`, `"The root cause was..."`, `"Note for future:"`
     - Cargo error patterns that were resolved (bug workarounds)
     - File paths that were repeatedly read (architecture knowledge)
   - **V2 (LLM extraction)**: Use a fast model to extract learnings. Prompt:
     ```
     Extract 0-3 stable learnings from this agent session output.
     Each learning should be a single sentence categorized as:
     pattern, architecture, bug, convention, integration, or performance.
     Only extract facts that would be useful to OTHER agents working on
     the same codebase. Do not extract task-specific progress.
     ```
3. For each extracted learning, send `/knowledge add <category> <content>` to the room
4. Other agents see the proposal and can `/knowledge confirm` entries they independently
   verify

**Cost**: V2 extraction using haiku costs ~$0.002 per session. At 5-10 sessions per
sprint per agent, this is ~$0.10/sprint — negligible.

**Safeguards**:
- Extraction is advisory — entries are proposed, not auto-confirmed
- Rate limit: max 3 entries per session to prevent spam
- Minimum session length: skip extraction for sessions under 1,000 output tokens
- Deduplication: check existing knowledge entries before proposing (substring match)

### 7.4 Cross-Agent Knowledge at Session Start

**Current state**: `build_prompt()` injects personality + progress + messages. Each agent
starts with no knowledge of what other agents have learned.

**Proposed**: At session start, ralph queries the KnowledgePlugin and injects relevant
entries into the prompt.

**Prompt assembly order** (updated from Area 4):

```
1. Personality text (if set)
2. AGENTS.md (shared instructions — Area 4 Layer 1)
3. Knowledge entries (shared memory — Area 4 Layer 2)
4. System context (room commands, rules)
5. Progress file L0 + L1 (personal state — Area 7.1)
6. Recent room messages
7. Task assignment
```

**Knowledge injection strategy**:

- At session start, ralph runs `room send <room> -t <token> /knowledge list` and
  parses the output
- All entries are injected as a `--- SHARED KNOWLEDGE ---` section
- If the knowledge base exceeds ~4,000 tokens, only inject confirmed entries (those
  with `confirmed_by` non-empty). Unconfirmed entries are available via `/knowledge search`
  but not auto-injected.
- Category filtering: for code tasks, prioritize `pattern`, `architecture`, `bug`.
  For coordination tasks, prioritize `convention`, `integration`.

### 7.5 Session Compression (Context Cycle Improvement)

**Current state**: When context exceeds 80% threshold, ralph restarts with a completely
fresh prompt. The only continuity is the progress file (raw last-50-lines).

**Proposed**: Improve the context cycle with structured handoff.

**Pre-restart sequence** (in `on_context_cycle()`):

1. Write L2 progress file (as today)
2. Generate L1 structured summary (see 7.1)
3. Generate L0 one-liner (see 7.1)
4. Extract knowledge entries from session (see 7.3)
5. Send status update to room with L0 summary

**Post-restart prompt** (in next iteration's `build_prompt()`):

1. Load L0 + L1 (not L2) — gives the new context a compressed view
2. Load shared knowledge (which may now include entries from the just-ended session)
3. Include explicit instruction: "You are continuing from a context restart. Your
   previous session's summary is in the PROGRESS sections above. The full session
   log is at `<L2-path>` if you need details."

This reduces continuity overhead from unbounded to ~2,100 tokens while preserving
more structured information than today's raw output dump.

### 7.6 Memory Evolution: Manual vs Automatic

OpenViking's `MemoryExtractor` is fully automatic — no agent action needed. Room's
approach should be hybrid:

| Mechanism | Trigger | Agent Involvement | Reliability |
|-----------|---------|-------------------|-------------|
| `/knowledge add` (manual) | Agent discovers something | Active — agent decides what to share | High — intentional |
| Session extraction (auto) | End of ralph iteration | Passive — extracted from output | Medium — may extract noise |
| Knowledge confirmation | Another agent verifies | Active — agent confirms | High — cross-validated |
| Knowledge decay | 30 days without confirmation | None — automatic audit flag | High — prevents stale entries |

The manual path (`/knowledge add`) is the primary mechanism. Automatic extraction (7.3)
is supplementary — it catches learnings the agent didn't explicitly share. Both feed
into the same KnowledgePlugin store.

**Why hybrid, not fully automatic?** Fully automatic extraction (OpenViking's approach)
works when there's a single agent with a single context. In a multi-agent system, automatic
extraction from every session would flood the knowledge base with redundant or conflicting
entries. Manual contribution with automatic supplementation is the right balance.

---

## Area 8: Integration Recommendation (Build vs Integrate vs Hybrid)

### Option A: Adopt OpenViking as External Service

Run OpenViking's Python server alongside the room broker. Ralph shells out to the OV CLI
or calls its HTTP API for context retrieval and memory persistence.

| Pros | Cons |
|------|------|
| L0/L1/L2 tiered loading out of the box | Python runtime dependency |
| Vector search for semantic retrieval | Operational overhead (separate server process) |
| Mature embedding pipeline | OpenClaw-centric, no Claude-native support |
| Active development by ByteDance | Early stage (v0.2.3), API may change |

**Verdict**: Not recommended at current scale. Revisit when managing 10+ concurrent agents
or when Hive needs centralized cross-workspace context.

### Option B: Full Native Rust Implementation

Build equivalent L0/L1/L2 tiered loading, vector search, and memory extraction entirely
in Rust within the room workspace.

| Pros | Cons |
|------|------|
| Pure Rust, no external dependencies | Large engineering effort (vector DB, embeddings) |
| Full control over data model | Reinventing well-solved problems |
| Tight integration with broker | Maintenance burden for embedding infrastructure |

**Verdict**: Not recommended. Building a vector database and embedding pipeline from
scratch is disproportionate to the problem. The valuable parts of OpenViking's design
don't require vector search.

### Option C: Hybrid — Adopt Concepts, Build Lightweight (Recommended)

Implement OpenViking's key concepts natively in Rust without its infrastructure:

1. **L0/L1/L2 tiered progress** (Area 7.1): Template-based extraction (V1), no vector
   search needed. Optional LLM summarization (V2) for higher quality.
2. **KnowledgePlugin with structured categories** (Area 7.2): NDJSON storage with
   category enum, substring search. No vector DB.
3. **Session-end extraction** (Area 7.3): Pattern matching (V1) or fast-model
   summarization (V2). Proposals, not auto-commits.
4. **Knowledge injection at session start** (Area 7.4): Simple category filtering
   and token budget, not semantic ranking.

**What this gives up vs full OpenViking:**
- No vector-based semantic search (substring search instead)
- No embedding-based similarity (category filtering instead)
- No directory recursive retrieval (flat category list instead)

**What this preserves:**
- Tiered loading (the highest-value concept)
- Structured memory categories
- Automatic extraction (simplified)
- Cross-agent knowledge sharing
- Pure Rust, zero external dependencies

**Implementation effort**: ~2-3 sprints across Areas 7.1-7.5. Can be phased:

| Phase | Scope | Effort | Value |
|-------|-------|--------|-------|
| Phase 1 | Progress file migration (7.1 V1 only: template L1/L0) | 1-2 days | Immediate — reduces context waste |
| Phase 2 | KnowledgePlugin with categories (7.2) | 3-5 days | Medium — enables cross-agent learning |
| Phase 3 | Knowledge injection in ralph (7.4) | 1-2 days | Medium — agents start informed |
| Phase 4 | Session extraction V1 (7.3 pattern matching) | 2-3 days | Low-medium — supplementary to manual |
| Phase 5 | LLM summarization V2 (7.1 + 7.3) | 2-3 days | High — quality improvement |
| Phase 6 | Context cycle improvement (7.5) | 1-2 days | High — better continuity |

**Verdict**: Recommended. Delivers 80% of OpenViking's value at 10% of its complexity.
The missing 20% (vector search, semantic retrieval) becomes relevant at a scale we haven't
reached yet and can be added later — potentially by integrating OpenViking at that point.

---

## Updated Implementation Priority

| Phase | Area | Effort | Dependencies |
|---|---|---|---|
| **P0** | Area 3 fix (mention reorder) | Small (code fix) | None — concrete bug |
| **P1** | Area 4 AGENTS.md | Small (file + ralph prompt change) | None |
| **P1** | Area 1 directory (tags on User) | Medium (registry + plugin) | None |
| **P2** | Area 7.1 tiered progress (V1) | Small (path change + template L1/L0) | Area 5 progress migration |
| **P2** | Area 4 KnowledgePlugin + 7.2 categories | Medium (new plugin) | Plugin system exists |
| **P2** | Area 7.4 knowledge injection in ralph | Small (prompt.rs change) | KnowledgePlugin |
| **P3** | Area 2 reputation | Medium (new plugin) | Directory (Area 1) |
| **P3** | Area 7.3 session extraction (V1) | Small-medium (pattern matching) | KnowledgePlugin |
| **P4** | Area 7.1 + 7.3 LLM summarization (V2) | Medium (haiku integration) | V1 phases |
| **P4** | Area 7.5 context cycle improvement | Small (loop_runner.rs) | Tiered progress |

P0 and P1 can ship in the current sprint. P2 is the next sprint's focus (shared memory
foundation). P3 adds sophistication. P4 introduces LLM-based quality improvements once
the pipeline is proven.

## Open Questions

1. ~~**Should the knowledge plugin enforce categories?**~~ Yes — fixed enum with six
   categories (see Area 7.2). New categories added via code change when demonstrated need.

2. **Should reputation labels affect spawn behavior?** e.g., `Probation` agents
   automatically get `--max-iter 1` or reduced tool access. This couples reputation
   to the personality system — may be too much for v1.

3. ~~**Should AGENTS.md be room-scoped or global?**~~ Global for now (project root),
   room-scoped override later. Confirmed by the memory hierarchy in Area 7.

4. **Should L1 generation be synchronous or async?** Synchronous (V1 template extraction)
   adds ~0ms to context cycle. Async (V2 LLM summarization) adds 2-5s but produces
   better summaries. Recommend V1 first, V2 as optional flag.

5. **Should knowledge entries have TTL?** OpenViking doesn't expire entries. Room's
   proposal (30-day unconfirmed entry audit) is a reasonable middle ground. Question:
   should confirmed entries also decay, or are they permanent?

6. **Should extracted knowledge require human approval?** Current proposal: extracted
   entries are proposed (unconfirmed) and other agents can confirm. Alternative: require
   host or ba approval for auto-extracted entries to prevent low-quality accumulation.
