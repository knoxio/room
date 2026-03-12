# PRD: Agent Discovery

> Status: Draft (revised)
> Author: r2d2 (original), saphire (revised)
> Date: 2026-03-12
> Dependencies: /who (shipped), /stats (shipped), /taskboard (shipped),
>   room-ralph (shipped)

## Problem

In a workspace with 10+ agents across multiple rooms, finding the right agent
for a task is non-trivial. Questions like "which agent knows about the broker
code?", "who has capacity right now?", and "which reviewer has the best track
record on CLI changes?" have no good answer today. The host manually assigns
work based on memory.

As the number of agents grows, manual assignment does not scale. Agents idle
while others are overloaded. Expertise is invisible — an agent that spent 20
iterations deep in `broker/mod.rs` has knowledge that is lost when it goes
offline.

## Goal

Provide an **agent discovery** system within Hive that indexes agent expertise,
tracks capacity, and helps users make informed assignment decisions through
Hive's web UI.

## Non-goals

- Autonomous task assignment without human approval (the coordinator or host
  decides — Hive suggests).
- Replacing the room coordination protocol.
- Training or fine-tuning agents based on history.

---

## Architecture

Agent discovery is a Hive-side feature. Room provides the raw data (status,
messages, taskboard state); Hive indexes, aggregates, and presents it.

```
Room (data sources)                    Hive (indexing + UI)
┌───────────────────┐                  ┌──────────────────────┐
│ /who (online)     │──[WS]──────────► │ Capacity tracker     │
│ /set_status       │──[WS]──────────► │                      │
│ /taskboard list   │──[WS/REST]─────► │ Assignment tracker   │
│ Chat history      │──[REST query]──► │                      │
└───────────────────┘                  │ Expertise indexer    │
                                       │   (also reads git)   │
Git history ──────────────────────────►│                      │
GitHub API ───────────────────────────►│ Skill matcher        │
                                       └──────────┬───────────┘
                                                   │
                                       ┌───────────▼──────────┐
                                       │  Hive Web UI         │
                                       │  - Agent profiles    │
                                       │  - Capacity view     │
                                       │  - Suggest agent     │
                                       └──────────────────────┘
```

---

## Core concepts

### 1. Expertise index

Every agent builds an implicit expertise profile through its work:

- **Files touched**: tracked from git history (`git log --author=<username>`).
- **Issues completed**: tracked from merged PRs with `Closes #N`.
- **Domains**: inferred from file paths (e.g. `broker/` = broker internals,
  `tui/` = terminal UI, `plugin/` = plugin system).

Hive builds and maintains the expertise index in its database:

```json
{
  "agent": "r2d2",
  "domains": {
    "broker": {"files_touched": 12, "prs_merged": 5, "last_active": "2026-03-12"},
    "oneshot": {"files_touched": 8, "prs_merged": 3, "last_active": "2026-03-10"},
    "ralph": {"files_touched": 6, "prs_merged": 4, "last_active": "2026-03-12"}
  },
  "total_prs": 12,
  "total_issues": 12,
  "avg_time_to_merge": "45m"
}
```

### 2. Capacity tracking

Capacity is derived from room's real-time data, streamed to Hive via WS:

- **Idle**: agent is online but has no active claim or taskboard assignment.
- **Active**: agent has claimed a task via `/taskboard claim` or `/claim`.
- **Blocked**: agent has set status containing "blocked" or "waiting".
- **Offline**: agent is not connected to any room.

The Hive dashboard shows a live capacity view:

```
 Agent      │ Status     │ Current task    │ Domains
 r2d2       │ idle       │ —               │ broker, oneshot, ralph
 saphire    │ active     │ #444 taskboard  │ tui, plugin
 bumblebee  │ active     │ #438 subs       │ broker, ws
 ba         │ monitoring │ sprint coord    │ all
```

### 3. Skill matching

Given a new issue, Hive suggests which agent is best suited:

1. Reads the issue title and body from GitHub (via webhook or API).
2. Extracts keywords and file path hints (e.g. "fix bug in broker/commands.rs").
3. Matches against the expertise index in Hive's database.
4. Filters by capacity (prefer idle agents over active ones).
5. Returns a ranked list of suggestions in the web UI.

```
Suggested agents for #450 (broker command routing bug):
  1. r2d2      — 12 files in broker/, 5 merged PRs, idle
  2. bumblebee — 4 files in broker/, 2 merged PRs, active (#438)
  3. saphire   — 1 file in broker/, 0 merged PRs, active (#444)
```

This is a suggestion, not an assignment. The user makes the final decision
through the web UI. Hive then sends the assignment via room message.

---

## User flows

### 1. Build the expertise index

Hive's backend periodically scans git history for all known agent usernames,
builds the domain map, and stores it in Hive's database. This can also be
triggered manually from the web UI ("Rebuild Index").

Incremental updates happen after each merged PR (detected via GitHub webhook).

### 2. View agent profiles

The web UI shows per-agent profile pages with:

- Expertise domains and depth (files touched, PRs merged)
- Current status and assignment
- Historical activity (PRs per sprint, test contribution)
- Cost data (API usage, session hours)

### 3. Find agents by domain

The web UI provides a search/filter interface:

- Filter by domain (broker, tui, plugin, etc.)
- Filter by status (idle, active, blocked)
- Sort by expertise depth, availability, or cost efficiency

### 4. Agent reputation (stretch goal)

Quality metrics per agent over time:

- **Merge rate**: PRs merged vs. PRs opened.
- **Review cycles**: average review rounds before merge.
- **Regression rate**: bugs introduced by merged PRs.
- **Test contribution**: net change in test count per PR.

Reputation data is visible to workspace admins in the web UI. It helps
decide whether an agent needs supervision or can work autonomously.

---

## Data model

### Hive database (not room)

All agent discovery state lives in Hive's database:

- **Expertise index**: per-agent domain map, built from git history.
- **Agent profiles**: expertise + reputation + cost data per agent.
- **Capacity snapshots**: periodic snapshots for trend analysis.

Room's data model is unchanged. Room provides raw status/message data;
Hive aggregates it.

---

## Room features used

| Feature | Status | How Hive uses it |
|---|---|---|
| /who | Shipped | Online/offline detection (via WS) |
| /stats | Shipped | Agent activity metrics |
| /set_status | Shipped | Capacity inference (via WS status stream) |
| /taskboard | Shipped | Current assignments |
| UserRegistry | Shipped | Agent identity across rooms |
| WS streaming | Shipped | Real-time status updates |
| REST query | Shipped | Historical message search |

---

## External dependencies

| Dependency | Purpose |
|---|---|
| Git history | Expertise index source data |
| GitHub API / webhooks | Issue details for skill matching, PR merge events |

---

## Resolved questions

1. **Expertise indexing is automatic.** Hive's backend rebuilds incrementally
   on PR merge events (GitHub webhook). Full rebuilds can be triggered from
   the web UI.

2. **Agent reputation is useful but admin-only.** Visible to workspace admins,
   not to agents themselves. Avoids perverse incentives.

3. **Expertise is global, not workspace-scoped.** Expertise comes from git
   history which is repo-scoped, not workspace-scoped. An agent's expertise
   from workspace A is visible when assigned to workspace B.

4. **Suggest-agent is advisory.** Hive suggests; humans decide. Auto-assignment
   is a future feature that requires explicit opt-in per workspace.
