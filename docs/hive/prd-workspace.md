# PRD: Hive Workspaces

> Status: Draft
> Author: r2d2
> Date: 2026-03-12
> Dependencies: daemon multi-room (shipped), UserRegistry (shipped), /taskboard (#444)

## Problem

Today, managing multiple rooms requires running separate `room` commands per room
or a daemon with manual `room create` / `room subscribe` invocations. There is no
concept of a "workspace" — a named collection of rooms with shared configuration,
team membership, and a unified view of activity across rooms.

For a team running 5-10 rooms (one per feature, one for triage, one for CI
notifications), the cognitive overhead of subscribing to the right rooms, tracking
which agent is in which room, and finding messages across rooms is significant.

## Goal

Provide a **workspace** abstraction that groups rooms, manages cross-room
subscriptions, and gives users a single entry point for multi-room workflows.

## Non-goals

- Replacing the daemon — workspaces are a layer on top of daemon rooms.
- Cross-host workspaces (see open questions).
- Room-level ACLs beyond what room already supports.

---

## User flows

### 1. Create a workspace

```bash
room hive workspace create sprint-12 \
  --rooms triage,feature-auth,feature-taskboard,ci-notifications \
  --default-room triage
```

This:
1. Ensures all listed rooms exist in the daemon (creates missing ones).
2. Stores workspace metadata in `~/.room/workspaces/sprint-12.toml`.
3. Auto-subscribes the user to all listed rooms.

```toml
# ~/.room/workspaces/sprint-12.toml
[workspace]
name = "sprint-12"
default_room = "triage"
rooms = ["triage", "feature-auth", "feature-taskboard", "ci-notifications"]
created_at = "2026-03-12T10:00:00Z"
```

### 2. Join a workspace

```bash
room hive workspace join sprint-12
```

Reads the workspace TOML, subscribes the user to all rooms, and opens the TUI
with the default room selected. The room list in the TUI sidebar shows only
workspace rooms (not all daemon rooms).

### 3. Multi-room view

```bash
room hive inbox
# or within TUI: /inbox
```

Shows recent messages from all workspace rooms, merged by timestamp:

```
[triage]          ba: sprint 12 scorecard updated
[feature-auth]    saphire: PR #445 merged
[ci-notifications] ci-bot: all checks passed on master
[triage]          joao: @r2d2 pick up #435
```

This is a read-only view. To reply, the user switches to the specific room.

Implementation: uses `room poll --rooms <list>` under the hood.

### 4. Add/remove rooms

```bash
room hive workspace add-room sprint-12 hotfix-queue
room hive workspace remove-room sprint-12 ci-notifications
```

Updates the TOML and adjusts subscriptions.

### 5. Archive a workspace

```bash
room hive workspace archive sprint-12
```

Unsubscribes from all rooms, moves the TOML to `~/.room/workspaces/archive/`.
Rooms are not destroyed — they persist in the daemon with their history.

---

## Team management within workspaces

Each workspace can have a **team roster** — named users and agents assigned to it.
The roster is informational (it does not enforce access control) but drives:

- Default agent spawning: `room hive team spawn sprint-12` spawns all agents
  defined in the team manifest (see prd-team-provisioning.md).
- Status dashboard: `room hive team status sprint-12` shows the status of every
  team member across all workspace rooms.
- Notifications: `room hive team notify sprint-12 "standup in 5"` sends a message
  to each team member via their subscribed room.

```toml
# Added to workspace TOML
[team]
members = ["joao", "ba", "r2d2", "saphire", "bumblebee"]
```

---

## Data model

### Workspace file

Location: `~/.room/workspaces/<name>.toml`

```toml
[workspace]
name = "sprint-12"
default_room = "triage"
rooms = ["triage", "feature-auth", "feature-taskboard"]
created_at = "2026-03-12T10:00:00Z"

[team]
members = ["joao", "ba", "r2d2", "saphire"]
```

### Workspace registry

Location: `~/.room/workspaces/registry.json`

```json
{
  "active": "sprint-12",
  "workspaces": ["sprint-12", "sprint-11"]
}
```

The `active` field tracks the last-used workspace for commands that don't specify
one explicitly.

---

## Dependencies on room features

| Feature | Status | How workspace uses it |
|---|---|---|
| Daemon multi-room | Shipped | Workspace rooms are daemon rooms |
| room create / destroy | Shipped | Workspace create ensures rooms exist |
| room subscribe | Shipped | Workspace join subscribes to all rooms |
| room poll --rooms | Shipped | Multi-room inbox view |
| UserRegistry | Shipped | Cross-room identity for team roster |
| /taskboard (#444) | In progress | Task tracking across workspace rooms |
| Subscription persistence (#438) | In progress | Survives broker restarts |

---

## Open questions for joao

1. **Should workspaces be shared or personal?** Current proposal: personal
   (each user has their own `~/.room/workspaces/`). Shared workspaces would
   require storing the TOML in the daemon's data directory and broadcasting
   changes.

2. **Should workspace rooms auto-create on workspace create?** Current proposal:
   yes — `workspace create` calls `room create` for any listed room that doesn't
   exist. Alternative: require rooms to exist first (fail-fast).

3. **TUI integration depth.** Options:
   - (a) Workspace-aware sidebar (show only workspace rooms, grouped).
   - (b) Full workspace TUI mode (tabs per room, unified notification).
   - (c) CLI-only for v1, TUI later.

4. **Workspace-scoped taskboard.** Should `/taskboard` commands be workspace-wide
   (tasks visible across all rooms) or per-room? Per-room is simpler but
   fragments task tracking.
