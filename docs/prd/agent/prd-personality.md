# PRD: Unified Personality System

## Status

Draft — 2026-03-12

## Problem

Agent configuration is split across two concepts: **profiles** (tool restriction
presets in room-ralph) and **personalities** (prompt + behavior presets for `/spawn`).
This creates confusion: which one do you configure? What's the relationship? The host
has to understand both to set up an agent correctly.

Additionally, agent behavior goes beyond tools and prompts. Agents need workflow
definitions (what to do when idle, how to handle tasks, when to stop), health
configuration (status update frequency, stale detection thresholds), and lifecycle
semantics (persistent vs ephemeral). None of these are captured by the current
profile/personality split.

## Goals

1. **Unified data type**: Replace the profile/personality split with a single
   `Personality` type that is the sole way to define agent behavior.
2. **Comprehensive configuration**: A personality captures everything needed to spawn
   and manage an agent — model, tools, prompt, workflow, health, naming.
3. **Layered resolution**: Built-in defaults ship with the binary. User-defined TOML
   files override built-ins. Hive (when present) manages personalities through its
   own storage.
4. **Fun naming**: Optional name pools per personality for human-friendly agent names
   instead of `reviewer-a1b2`.

## Non-goals

- Runtime personality hot-reload (agents restart to pick up changes).
- Personality versioning or migration.
- Personality sharing across machines (Hive handles this in the product layer).

## Design

### Personality Schema

A personality is a named configuration bundle with the following fields:

```toml
[personality]
name = "dev-coder"
description = "Full development agent — reads, writes, tests, commits"
lifecycle = "persistent"       # "persistent" or "ephemeral"
model = "opus"                 # claude model to use

[tools]
allow = []                     # tool allow list (empty = use defaults)
disallow = []                  # tool disallow list (hard-blocks)
allow_all = false              # bypass all tool restrictions

[prompt]
template = """
You are a development agent. Your workflow:
1. Poll the taskboard for available tasks
2. Claim a task and announce your plan
3. Implement, test, and open a PR
4. Announce completion and return to idle
"""

[workflow]
on_idle = "poll taskboard, claim if available"
on_task = [
    "read target files",
    "announce plan in room",
    "wait for approval",
    "implement",
    "run tests",
    "open PR",
    "announce completion"
]
on_done = "release task, return to idle"

[health]
status_interval_max = "5m"     # max time between /set_status updates
stale_threshold = "10m"        # agent is flagged stale after this silence

[naming]
name_pool = ["anna", "kai", "nova", "zara", "leo", "mika", "juno", "reo"]
```

### Field Reference

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Unique identifier, used in `/spawn <name>` |
| `description` | string | yes | Human-readable purpose, shown in `/spawn list` |
| `lifecycle` | enum | yes | `persistent` (runs until stopped) or `ephemeral` (exits on task completion) |
| `model` | string | no | Claude model ID. Default: inherited from room-ralph default |
| `tools.allow` | string[] | no | Tool allow list passed to `--allow-tools` |
| `tools.disallow` | string[] | no | Tool disallow list passed to `--disallow-tools` |
| `tools.allow_all` | bool | no | If true, bypass all tool restrictions |
| `prompt.template` | string | yes | System prompt prepended to all agent interactions |
| `workflow.on_idle` | string | no | Behavior description when no task is assigned |
| `workflow.on_task` | string[] | no | Ordered steps the agent follows during task execution |
| `workflow.on_done` | string | no | Behavior after task completion |
| `health.status_interval_max` | duration | no | Max time between status updates before flagged |
| `health.stale_threshold` | duration | no | Time after which agent is considered stale |
| `naming.name_pool` | string[] | no | Pool of names for auto-generated usernames |

### Lifecycle Types

**Persistent agents** run indefinitely. They poll for work, execute tasks, and return
to idle. Examples: development agents, coordinator agents, QA agents. Their "definition
of done" is never — they keep listening to the room stream and claiming tasks.

**Ephemeral agents** are spawned for a specific task and exit when done. Examples: PR
reviewers (review one PR, post feedback, exit), summarizers (summarize a thread, post
result, exit). Their "definition of done" is completing the assigned task.

The lifecycle type affects:
- **Health monitoring**: Persistent agents must maintain regular status updates.
  Ephemeral agents are expected to finish quickly — staleness is measured against
  expected task duration, not an ongoing heartbeat.
- **Orphan cleanup**: Persistent agents are only cleaned up on broker shutdown or
  explicit `/agent stop`. Ephemeral agents are cleaned up when they exit (and flagged
  if they run longer than expected).
- **Taskboard interaction**: Persistent agents poll and claim tasks autonomously.
  Ephemeral agents are spawned with a pre-assigned task.

### Name Pools

When a personality has a `name_pool`, `/spawn <personality>` picks a random name from
the pool instead of generating `<personality>-<uuid>`. Format: `<personality>-<name>`
(e.g. `dev-anna`, `reviewer-kai`).

Rules:
- Names are drawn without replacement within a room (no two active agents share a name).
- If all names in the pool are in use, fall back to `<personality>-<short-uuid>`.
- The host can override with an explicit username: `/spawn reviewer --name custom-name`.
- Name pools are optional. If `name_pool` is empty or absent, UUID suffix is used.

Example name pools by personality type:
- **Coder**: developer-themed names
- **Reviewer**: analyst-themed names
- **Coordinator**: leader-themed names

### Resolution Order

1. User-defined TOML at `~/.room/personalities/<name>.toml`
2. Built-in defaults compiled into the binary
3. (Future) Hive-managed personalities via API

User-defined TOML files fully replace built-ins with the same name (no merging).

### Built-in Defaults

The following personalities ship with the binary:

| Name | Lifecycle | Model | Tools | Description |
|---|---|---|---|---|
| `coder` | persistent | opus | full access | Development agent — reads, writes, tests, commits |
| `reviewer` | ephemeral | sonnet | disallow Write, Edit | PR reviewer — read-only code access, gh commands |
| `scout` | ephemeral | haiku | disallow Write, Edit, Bash | Codebase explorer — search and summarize only |
| `qa` | persistent | sonnet | full access | Test writer — finds coverage gaps, writes tests |
| `coordinator` | persistent | sonnet | disallow Write, Edit | BA/triage — reads code, manages issues, coordinates |

### Relationship to room-ralph Profiles

The existing `Profile` enum in room-ralph (`Coder`, `Reviewer`, `Coordinator`, `Notion`,
`Reader`) maps to tool restriction presets. With unified personalities, profiles become
an implementation detail:

- When room-ralph is invoked directly (CLI), the `--profile` flag still works as a
  shorthand for tool restrictions.
- When spawned via `/agent` or `/spawn`, the personality's `tools` section replaces
  the profile entirely. The `--profile` flag is not passed.
- Over time, profiles can be deprecated in favor of personality TOML files.

### Hive Integration

When Hive is present, it owns personality management:
- Personalities are workspace-scoped (team resources), not global files.
- Hive stores personalities in its own database, not `~/.room/personalities/`.
- Hive passes the full personality config to `/agent spawn` or directly to room-ralph.
- The built-in defaults and TOML files serve as the standalone (non-Hive) experience.

## Implementation Notes

### Files

- `crates/room-daemon/src/plugin/agent.rs` — personality registry, TOML loading, built-in
  defaults
- `crates/room-ralph/src/personalities.rs` — built-in personality definitions (already
  exists for profiles, extend with full personality schema)

### Data Model (Rust)

```rust
pub struct Personality {
    pub name: String,
    pub description: String,
    pub lifecycle: LifecycleType,
    pub model: Option<String>,
    pub tools: ToolConfig,
    pub prompt: PromptConfig,
    pub workflow: WorkflowConfig,
    pub health: HealthConfig,
    pub naming: NamingConfig,
}

pub enum LifecycleType {
    Persistent,
    Ephemeral,
}

pub struct ToolConfig {
    pub allow: Vec<String>,
    pub disallow: Vec<String>,
    pub allow_all: bool,
}

pub struct WorkflowConfig {
    pub on_idle: Option<String>,
    pub on_task: Vec<String>,
    pub on_done: Option<String>,
}

pub struct HealthConfig {
    pub status_interval_max: Option<Duration>,
    pub stale_threshold: Option<Duration>,
}

pub struct NamingConfig {
    pub name_pool: Vec<String>,
}
```

### Tests

- TOML deserialization round-trip for all field types
- Resolution order: user TOML overrides built-in
- Name pool: draw without replacement, fallback to UUID on exhaustion
- Lifecycle type affects spawn behavior (persistent vs ephemeral flags)
- Built-in defaults match expected configurations
