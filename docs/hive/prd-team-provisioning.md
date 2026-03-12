# PRD: Team Provisioning

> Status: Draft (revised)
> Author: r2d2 (original), saphire (revised)
> Date: 2026-03-12
> Dependencies: room-ralph (shipped), workspaces (prd-workspace.md)

## Problem

Spawning a team of agents today is a manual process. The host runs multiple
`room-ralph` commands with different personalities, models, and issue
assignments. If agents crash, ralph handles restarts — but provisioning the
initial team and managing its lifecycle (scale up, tear down, re-provision)
has no unified interface.

As Hive manages larger deployments, the manual approach does not scale.
Hive needs a programmatic way to define, spawn, monitor, and tear down
agent teams.

## Goal

Let Hive define **team manifests** — declarative descriptions of agent teams
that can be provisioned, monitored, and torn down through Hive's web interface
and API.

## Non-goals

- Modifying room-ralph internals — ralph remains an independent agent execution
  tool. Hive orchestrates it, not replaces it.
- Resource scheduling across multiple hosts (future aspiration).
- Agent-to-agent delegation or hierarchy at the protocol level.

---

## Architecture

Agent lifecycle management is its own domain within Hive. The stack:

```
Hive Web UI
    │
    ▼
Hive Server (orchestration: decides when/why to spawn)
    │
    ▼
Agent Runner (lifecycle: clone, workdir setup, spawn, monitor, restart)
    │
    ▼
room-ralph (execution: prompt building, context monitoring, claude interaction)
    │
    ▼
room (collaboration: messages, plugins, persistence)
```

Whether the Agent Runner is a separate crate or a module within Hive is an
implementation decision deferred to development. The PRD captures
responsibilities and interfaces, not package structure.

### Responsibilities

| Component | Owns |
|---|---|
| Hive Server | Business logic — "PR #42 arrived, spin up a reviewer" |
| Agent Runner | Infrastructure — clone repo at PR ref, set up workdir, spawn ralph, monitor process, handle restarts |
| room-ralph | Agent behavior — prompts, tool selection, context monitoring, claude subprocess |
| room | Collaboration — message routing, plugins, persistence |

---

## User flows

All interactions happen through Hive's web UI or API.

### 1. Define a team manifest

In Hive's web UI, the user creates a team template:

```json
{
  "name": "sprint-dev",
  "description": "Standard sprint development team",
  "agents": [
    {
      "personality": "coder",
      "username_prefix": "coder",
      "model": "opus",
      "count": 2
    },
    {
      "personality": "reviewer",
      "username_prefix": "reviewer",
      "model": "sonnet"
    },
    {
      "personality": "coordinator",
      "username_prefix": "ba",
      "model": "opus"
    }
  ]
}
```

Manifests are stored in Hive's database. They are reusable across
workspaces and sprints.

### 2. Provision a team

From the workspace view, the user clicks "Spawn Team" and selects a
manifest. Hive:

1. Reads the manifest from its database.
2. For each agent entry, the Agent Runner:
   - Optionally clones the target repo at the specified ref.
   - Sets up an isolated workdir.
   - Mints a room token (via `room join`), stored in Hive's database.
   - Subscribes the agent to the workspace rooms.
   - Spawns `room-ralph` with the appropriate profile, model, and room token.
3. Broadcasts a team provisioning summary to the workspace's default room.

### 3. Monitor team status

The workspace dashboard shows a live agent status panel:

```
Team: sprint-dev (workspace: sprint-12)
 Agent       │ Personality │ Model  │ Status             │ Uptime
 coder-a1b2  │ coder       │ opus   │ implementing #430  │ 14m
 coder-c3d4  │ coder       │ opus   │ running tests      │ 12m
 reviewer-e5 │ reviewer    │ sonnet │ reviewing PR #443  │ 8m
 ba-g7h8     │ coordinator │ opus   │ monitoring sprint  │ 14m
```

Status is derived from room's `/who` output and agent `/set_status` values,
streamed to Hive via WS.

### 4. Scale a team

From the dashboard, the user can add or remove agents:

- **Add**: spawn another agent of a given personality, assigned to a specific
  issue or task.
- **Remove**: gracefully stop a specific agent (sends shutdown signal, waits
  for current work to complete, broadcasts leave event).

### 5. Tear down a team

"Stop Team" sends shutdown signals to all agents, collects final status, and
broadcasts a summary. The manifest is preserved for re-use. Agent tokens are
revoked.

### 6. Automated provisioning (advanced)

Hive can trigger agent provisioning based on external events:

- **PR arrives**: Hive detects a new PR via GitHub webhook, clones the repo
  at the PR ref, spawns a reviewer agent with the repo as workdir.
- **Issue created**: Hive detects a new issue, matches it to an available
  agent via skill matching (see prd-agent-discovery.md), and assigns it.
- **Schedule**: spawn a CI-watcher agent at the start of each business day,
  tear it down at end of day.

---

## Billing and metering

Hive tracks per-agent costs in detail:

| Cost type | Source | Tracking |
|---|---|---|
| Cloud API (Claude, etc.) | API provider billing | Per-agent, per-session |
| Local model | Compute time | Per-agent, per-session |
| Infrastructure | room server resources | Per-workspace |

Agents may be mixed: a coding agent uses Claude (cloud billing), while a
summarization agent runs a local model (no API cost, but compute cost). Hive
must distinguish these and provide per-agent and per-team cost rollups.

The billing UI shows:

- Per-agent cost breakdown (API calls, tokens consumed, compute time)
- Per-team aggregates
- Per-workspace totals
- Historical trends

---

## Data model

### Hive database (not room)

Team state lives in Hive's database:

- **Team manifests**: reusable templates (name, agents, personalities, models)
- **Active team instances**: running agents (username, PID, spawn time, room
  tokens, assigned issues)
- **Billing records**: per-agent cost events (API calls, duration, model used)

Room's data model is unchanged. Room only knows about connected users and
messages.

---

## Room features used

| Feature | Status | How Hive uses it |
|---|---|---|
| Token issuance (room join) | Shipped | Mint tokens for spawned agents |
| room subscribe | Shipped | Auto-subscribe agents to workspace rooms |
| /who + status | Shipped | Live agent status for dashboard |
| /taskboard | Shipped | Task assignment tracking |
| WS streaming | Shipped | Real-time status updates |
| Token persistence | Shipped | Tokens survive broker restarts |

---

## Resolved questions

1. **Issue assignment**: Hive manages issue assignment, not the team manifest.
   Manifests define agent types; Hive's orchestration layer assigns specific
   issues based on capacity and expertise (see prd-agent-discovery.md).

2. **Resource limits**: Hive enforces per-workspace and per-team agent limits
   in its own configuration, not in room.

3. **Restart semantics**: room-ralph handles individual agent restarts
   (context exhaustion, errors). Hive handles team-level lifecycle (provision,
   scale, tear down). Progress files bridge the gap — ralph reads them on
   restart, Hive can manage them via the Agent Runner.

4. **Shared manifests**: manifests are stored in Hive's database, accessible
   to all team members. No per-user local files.
