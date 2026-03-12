# PRD: Hive Workspaces

> Status: Draft (revised)
> Author: r2d2 (original), saphire (revised)
> Date: 2026-03-12
> Dependencies: daemon multi-room (shipped), UserRegistry (shipped), /taskboard (shipped)

## Problem

Teams running multiple rooms (one per feature, one for triage, one for CI
notifications) face cognitive overhead: tracking which agent is in which room,
finding messages across rooms, and managing subscriptions. There is no unified
"workspace" concept that groups rooms with shared configuration and team
membership.

## Goal

Provide a **workspace** abstraction within Hive that groups rooms, manages
team membership, and gives users a unified view of activity across rooms.

## Non-goals

- Replacing the room daemon — workspaces are a Hive-level concept layered on
  daemon rooms.
- Cross-host workspaces (future aspiration — requires WS relay).
- Room-level ACLs beyond what room already supports.

---

## Architecture

Workspaces are a **Hive server concept**, not a room concept. Room has no
knowledge of workspaces — it only knows about individual rooms and their
subscribers. Hive maintains workspace state in its own database and
translates workspace operations into room API calls.

```
Hive Web UI                    Hive Server                Room Daemon
┌──────────────┐               ┌──────────────┐           ┌──────────┐
│ Workspace    │──[HTTP/WS]──► │ Workspace    │──[WS]───► │ Room A   │
│   Dashboard  │               │   Manager    │──[WS]───► │ Room B   │
│              │               │              │──[REST]──► │ Room C   │
└──────────────┘               └──────────────┘           └──────────┘
```

## User flows

All interactions happen through Hive's web UI or native app. There are no
CLI commands — Hive is not a CLI tool.

### 1. Create a workspace

The user opens Hive's web dashboard, creates a workspace named "sprint-12",
and selects which rooms to include. Hive:

1. Ensures all listed rooms exist in the bundled room daemon (creates missing
   ones via REST `POST /api/<room>/create`).
2. Stores workspace metadata in Hive's database.
3. Auto-subscribes the user to all listed rooms via room's subscribe API.

### 2. Workspace dashboard

The workspace view shows a unified timeline of messages from all workspace
rooms, merged by timestamp:

```
[triage]           ba: sprint 12 scorecard updated
[feature-auth]     saphire: PR #445 merged
[ci-notifications] ci-bot: all checks passed on master
[triage]           joao: @r2d2 pick up #435
```

Implementation: Hive maintains WS connections to each workspace room (or a
single multiplexed connection once room supports it) and merges the streams
in real-time.

### 3. Room switching

From the workspace view, the user clicks on a room name to enter that room's
dedicated view. The room view shows full chat history, member list, and
allows sending messages — all via room's WS transport.

### 4. Add/remove rooms

The workspace settings panel allows adding or removing rooms. Hive updates
its database and adjusts room subscriptions accordingly.

### 5. Archive a workspace

Archiving a workspace unsubscribes users from all rooms and hides the
workspace from the active list. Rooms are not destroyed — they persist in
the daemon with their history.

---

## Team management within workspaces

Each workspace has a **team roster** — named users and agents assigned to it.

- **Status dashboard**: the workspace view shows each team member's status
  across all workspace rooms (pulled from room's `/who` and status data via
  WS).
- **Agent spawning**: the workspace provides a "spawn team" action that
  provisions agents according to a team manifest (see prd-team-provisioning.md).
- **Notifications**: Hive can broadcast a message to all team members via
  their subscribed rooms.

---

## Data model

### Hive database (not room)

Workspace state lives in Hive's database — not in room's filesystem or
NDJSON files. This includes:

- Workspace name, creation date, archived status
- List of room IDs in the workspace
- Team roster (user/agent names, roles)
- Active workspace per user (last used)

Room's data model is unchanged. Room only knows about individual rooms,
subscribers, and messages.

---

## Room features used

| Feature | Status | How Hive uses it |
|---|---|---|
| Daemon multi-room | Shipped | Workspace rooms are daemon rooms |
| room create / destroy | Shipped | Workspace create ensures rooms exist (via REST) |
| room subscribe | Shipped | Workspace join subscribes to all rooms |
| WS streaming | Shipped | Real-time message feed for workspace view |
| REST poll/query | Shipped | History and search across workspace rooms |
| UserRegistry | Shipped | Cross-room identity for team roster |
| /taskboard | Shipped | Task tracking across workspace rooms |
| Subscription persistence | Shipped | Survives broker restarts |

---

## Resolved questions

1. **Workspaces are shared, managed by Hive.** Since Hive is a server, workspace
   state is centralized in Hive's database — not per-user local TOML files.
   All team members see the same workspace configuration.

2. **Workspace rooms auto-create.** When a workspace is created, Hive ensures
   all listed rooms exist in the daemon, creating any that are missing.

3. **Web UI, not TUI.** Hive provides a web dashboard. The room TUI remains
   independently useful for direct room access but is not part of Hive.

4. **Taskboard scope.** Per-room taskboards remain the default (room is
   generic). Hive's web UI can aggregate taskboard data across workspace rooms
   by querying each room's `/taskboard list` response.
