# PRD: Agent Discovery

> Status: Draft
> Author: r2d2
> Date: 2026-03-12
> Dependencies: /who (shipped), /stats (shipped), personalities (#439),
>   /taskboard (#444)

## Problem

In a workspace with 10+ agents across multiple rooms, finding the right agent
for a task is non-trivial. Questions like "which agent knows about the broker
code?", "who has capacity right now?", and "which reviewer has the best track
record on CLI changes?" have no good answer today. The host manually assigns
work based on memory and gut feel.

As the number of agents grows, manual assignment doesn't scale. Agents idle
while others are overloaded. Expertise is invisible — an agent that spent 20
iterations deep in `broker/mod.rs` has knowledge that is lost when it goes
offline.

## Goal

Provide an **agent discovery** system that indexes agent expertise, tracks
capacity, and helps the host (or coordinator agent) make informed assignment
decisions.

## Non-goals

- Autonomous task assignment (agents don't self-assign — the host or coordinator
  decides).
- Replacing the room coordination protocol.
- Training or fine-tuning agents based on history.

---

## Core concepts

### 1. Expertise index

Every agent builds an implicit expertise profile through its work:

- **Files touched**: tracked from git history (`git log --author=<username>`).
- **Issues completed**: tracked from merged PRs with `Closes #N`.
- **Domains**: inferred from file paths (e.g. `broker/` → broker internals,
  `tui/` → terminal UI, `tests/` → testing infrastructure).

The expertise index is a per-agent record stored in the workspace state:

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

Capacity is derived from agent status and current assignments:

- **Idle**: agent is online but has no active claim or issue.
- **Active**: agent has claimed a task via `/taskboard claim` or `/claim`.
- **Blocked**: agent has set status containing "blocked" or "waiting".
- **Offline**: agent is not connected to any room.

```bash
room hive agents --capacity
```

```
 agent      | status     | current task   | domains
 r2d2       | idle       | —              | broker, oneshot, ralph
 saphire    | active     | #444 taskboard | tui, plugin
 bumblebee  | active     | #438 subs      | broker, ws
 ba         | monitoring | sprint coord   | all
```

### 3. Skill matching

Given a new issue, suggest which agent is best suited:

```bash
room hive suggest-agent --issue 450
```

The system:
1. Reads the issue title and body from GitHub (`gh issue view 450`).
2. Extracts keywords and file path hints (e.g. "fix bug in broker/commands.rs").
3. Matches against the expertise index.
4. Filters by capacity (prefer idle agents over active ones).
5. Returns a ranked list of suggestions.

```
Suggested agents for #450 (broker command routing bug):
  1. r2d2     — 12 files in broker/, 5 merged PRs, idle
  2. bumblebee — 4 files in broker/, 2 merged PRs, active (#438)
  3. saphire   — 1 file in broker/, 0 merged PRs, active (#444)
```

This is a suggestion, not an assignment. The host or coordinator makes the
final decision.

---

## User flows

### 1. Build the expertise index

```bash
room hive agents index
```

Scans git history for all known agent usernames, builds the domain map, and
writes to `~/.room/state/expertise.json`. This is an offline operation that
can be run periodically or triggered after sprints close.

For incremental updates:

```bash
room hive agents index --since 2026-03-01
```

### 2. Query agent expertise

```bash
room hive agents expertise r2d2
```

```
Agent: r2d2
Total PRs: 12 | Total issues: 12 | Avg time to merge: 45m

Domains:
  broker/     12 files, 5 PRs (last: 2026-03-12)
  oneshot/     8 files, 3 PRs (last: 2026-03-10)
  ralph/       6 files, 4 PRs (last: 2026-03-12)
  protocol/    2 files, 1 PR  (last: 2026-02-28)
```

### 3. Find agents by domain

```bash
room hive agents find --domain tui
```

```
Agents with TUI expertise:
  saphire    — 15 files, 6 PRs (last: 2026-03-12)
  bumblebee  — 3 files, 1 PR (last: 2026-03-05)
```

### 4. Agent reputation (stretch goal)

Track quality metrics per agent over time:

- **Merge rate**: PRs merged vs. PRs opened (indicates clean implementation).
- **Review cycles**: average number of review rounds before merge.
- **Regression rate**: bugs introduced by merged PRs (tracked by
  `git bisect` or issue references).
- **Test contribution**: net change in test count per PR.

```bash
room hive agents reputation r2d2
```

```
Agent: r2d2 (sprint 10-12)
  Merge rate:      100% (12/12)
  Avg review cycles: 1.2
  Regressions:     1 (#411 caused by #408)
  Test contribution: +42 net tests
```

This data helps the coordinator decide whether an agent needs supervision
(pair with a reviewer) or can be trusted with autonomous work.

---

## Data model

### Expertise index

Location: `~/.room/state/expertise.json`

Built from git history. Updated on-demand via `room hive agents index`.

### Agent profiles

Location: `~/.room/state/agent-profiles/<username>.json`

Per-agent profile including expertise, reputation, and preferences. Written
by the index command and updated incrementally.

### Capacity snapshot

Not persisted — derived live from:
- `/who` output (online/offline).
- Agent status (`/set_status` values).
- `/taskboard` claims (active assignments).

---

## Dependencies on room features

| Feature | Status | How agent discovery uses it |
|---|---|---|
| /who | Shipped | Online/offline detection |
| /stats | Shipped | Agent activity metrics |
| /set_status | Shipped | Capacity inference (idle/active/blocked) |
| /taskboard (#444) | In progress | Current assignments |
| Agent status tracking | Shipped | Real-time status in capacity view |
| UserRegistry | Shipped | Agent identity across rooms |
| Personalities (#439) | Shipped | Personality metadata in profiles |
| Git history | External | Expertise index source data |
| GitHub API (gh) | External | Issue details for skill matching |

---

## Implementation considerations

### Data freshness

The expertise index is built from git history — it is always stale by the
duration of the current sprint. For accurate suggestions, run
`room hive agents index` at the start of each sprint and after each major
merge batch.

Capacity is live — it reads current `/who` and status data. No staleness
concern.

### Privacy

Agent expertise data is derived from public git history and room messages. No
new data is collected — the index is a materialized view of existing information.

### Performance

Git history scanning for ~1000 commits takes <1 second. The expertise index
is small (KB range). No performance concern for workspaces with <50 agents.

---

## Open questions for joao

1. **Should expertise indexing be automatic?** Current proposal: manual
   (`room hive agents index`). Alternative: daemon runs periodic indexing in
   the background. Risk: resource usage on large repos.

2. **Agent reputation — useful or premature?** Reputation metrics (merge rate,
   regression rate) are valuable for the coordinator but could create perverse
   incentives if agents optimize for metrics over quality. Should this be
   host-only data?

3. **Cross-workspace expertise.** If an agent works in workspace A and then
   is assigned to workspace B, should its expertise from A be visible? Current
   proposal: expertise is global (not workspace-scoped) since it is derived
   from git history which is repo-scoped.

4. **Suggest-agent automation.** Should the coordinator agent be able to call
   `suggest-agent` programmatically and auto-assign, or should it always be a
   human decision? The coordination protocol today requires host approval for
   assignments — auto-assignment would bypass that.
