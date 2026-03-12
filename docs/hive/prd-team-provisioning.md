# PRD: Team Provisioning

> Status: Draft
> Author: r2d2
> Date: 2026-03-12
> Dependencies: /spawn + personalities (#434, #439), workspaces (prd-workspace.md)

## Problem

Spawning a team of agents today is a manual, repetitive process. For a typical
sprint, the host runs 3-5 `room-ralph` commands with different personalities,
models, and issue assignments. If the broker restarts, agents die and must be
re-spawned individually. There is no declarative way to say "I want a team of
coder + reviewer + coordinator in this workspace" and have it materialize.

## Goal

Let users define **team manifests** — declarative descriptions of agent teams
that can be provisioned with a single command. Teams are ephemeral (spawn, use,
tear down) but the manifest is reusable.

## Non-goals

- Persistent agent processes (agents still die on broker restart — ralph handles
  reconnection).
- Agent-to-agent delegation or hierarchy.
- Resource scheduling across multiple hosts.

---

## User flows

### 1. Define a team manifest

```toml
# ~/.room/teams/sprint-dev.toml
[team]
name = "sprint-dev"
description = "Standard sprint development team"

[[agent]]
personality = "coder"
username_prefix = "coder"
model = "opus"
count = 2

[[agent]]
personality = "reviewer"
username_prefix = "reviewer"
model = "sonnet"

[[agent]]
personality = "coordinator"
username_prefix = "ba"
model = "opus"
```

Each `[[agent]]` entry maps to a `/spawn` invocation. The `count` field
(default 1) allows spawning multiple agents of the same personality.

### 2. Provision a team

```bash
room hive team spawn sprint-dev --room triage --issue-prefix 430
```

This:
1. Reads `~/.room/teams/sprint-dev.toml`.
2. For each agent entry, calls `/spawn` (or `room-ralph` directly):
   - `coder-a1b2` with personality=coder, model=opus, issue=430
   - `coder-c3d4` with personality=coder, model=opus, issue=431
   - `reviewer-e5f6` with personality=reviewer, model=sonnet
   - `ba-g7h8` with personality=coordinator, model=opus
3. Each agent joins the specified room automatically.
4. A summary is broadcast to the room.

### 3. Check team status

```bash
room hive team status sprint-dev
```

```
Team: sprint-dev (room: triage)
 agent       | personality | model  | status           | uptime
 coder-a1b2  | coder       | opus   | implementing #430 | 14m
 coder-c3d4  | coder       | opus   | running tests     | 12m
 reviewer-e5 | reviewer    | sonnet | reviewing PR #443 | 8m
 ba-g7h8     | coordinator | opus   | monitoring sprint | 14m
```

Uses `/agent list` and `/who` data combined with status information.

### 4. Tear down a team

```bash
room hive team stop sprint-dev
```

Sends `/agent stop` for every agent in the team. Broadcasts a shutdown
summary to the room. The manifest is preserved for re-use.

### 5. Scale a team

```bash
room hive team scale sprint-dev --add coder --issue 445
```

Adds one more coder agent to the running team, assigned to issue 445.

```bash
room hive team scale sprint-dev --remove reviewer-e5f6
```

Stops a specific agent from the team.

---

## Agent manifests (advanced)

For more control than personalities provide, users can define full agent
manifests that specify custom prompts, tool restrictions, and environment:

```toml
# ~/.room/teams/security-audit.toml
[team]
name = "security-audit"

[[agent]]
username_prefix = "auditor"
profile = "reader"
model = "opus"
prompt_file = "~/.room/prompts/security-audit.md"
disallow_tools = ["Write", "Edit", "Bash(git push *)"]
add_dirs = ["/path/to/target-repo"]
max_iter = 10
```

This gives full control over the `room-ralph` invocation without needing
a custom personality definition.

---

## Ephemeral teams

Teams spawned with `--ephemeral` auto-destroy when a condition is met:

```bash
room hive team spawn sprint-dev --room triage --ephemeral=idle:30m
```

Options:
- `--ephemeral=idle:30m` — tear down after 30 minutes of no agent activity.
- `--ephemeral=task:445` — tear down when issue #445 is closed.
- `--ephemeral=time:2h` — tear down after 2 hours regardless.

Ephemeral teams are tracked via a metadata file alongside the manifest. A
background check (in the daemon or a helper process) polls for the condition
and runs `team stop` when met.

---

## Data model

### Team manifest file

Location: `~/.room/teams/<name>.toml`

### Active team state

Location: `~/.room/state/teams/<name>.json`

```json
{
  "team": "sprint-dev",
  "room": "triage",
  "spawned_at": "2026-03-12T10:00:00Z",
  "agents": [
    {"username": "coder-a1b2", "pid": 12345, "personality": "coder", "issue": "430"},
    {"username": "reviewer-e5f6", "pid": 12400, "personality": "reviewer", "issue": null}
  ],
  "ephemeral": null
}
```

This file is written on spawn and updated on scale/stop. It is the source of
truth for `team status` and `team stop`.

---

## Dependencies on room features

| Feature | Status | How team provisioning uses it |
|---|---|---|
| /spawn + personalities | In progress (#434, #439) | Core spawn mechanism |
| /agent list + stop | Planned (#434 phase 1) | Team status and teardown |
| PID persistence | Planned (#434 phase 3) | Survive broker restarts |
| Agent status tracking | Shipped | Team status dashboard |
| Workspaces | Proposed (prd-workspace.md) | Team-workspace association |

---

## Open questions for joao

1. **Issue assignment strategy.** Should `--issue-prefix` auto-increment
   (430, 431, 432...) or should each agent entry in the manifest specify its
   own issue? Auto-increment is convenient but fragile (gaps in issue numbers).

2. **Team-level resource limits.** Should there be a max total agents across
   all teams per daemon (e.g. 20)? Or per-team limits in the manifest? Current
   `/agent` has a per-room limit of 5.

3. **Team restart semantics.** If an agent in a team crashes (context exhaustion,
   error), ralph already handles restart. But if the whole team is stopped and
   re-spawned (`team spawn` again), should it resume from progress files or
   start fresh? Progress files are per-issue, so they'd survive — but the team
   state file would need reconciliation.

4. **Shared vs. personal manifests.** Like workspaces, team manifests are
   currently personal (`~/.room/teams/`). Should there be a shared location
   (e.g. in the repo under `teams/`) so all team members use the same
   definition?
