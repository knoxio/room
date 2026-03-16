# PRD: Hive Frontend (Web Dashboard + Desktop)

**Status:** Draft
**Author(s):** samantha (based on wall-e UI investigation, r2d2 tech stack)
**Date:** 2026-03-16
**Dependencies:** Hive Server (prd-hive-server), room v3.5.1 (shipped)
**Breaks down:** #803 (UI investigation), #798 (tech stack — frontend portion)

---

## Problem

Room's TUI is terminal-only and single-room focused. There is no graphical interface for managing multiple rooms, monitoring agent teams, viewing task progress, or tracking costs. Non-technical users cannot interact with room-based agent teams.

## Goal

Build a web-based dashboard that provides:
1. Multi-room workspace view with unified timeline
2. Agent management UI (spawn, stop, monitor, logs)
3. Task board visualization
4. Real-time updates via WebSocket

Phase 2 wraps the same web app in Tauri for desktop features (tray icon, notifications, file system access).

## Non-goals

- Replacing the terminal TUI (it remains for power users)
- Mobile app (web is responsive enough for tablets)
- Offline mode
- Video/voice integration

## Architecture

### Tech stack (decided)

| Layer | Choice | Rationale |
|---|---|---|
| Framework | React or Svelte | Component-based, large ecosystem. TBD in spike. |
| Styling | Tailwind CSS | Utility-first, fast iteration |
| State management | Zustand or Svelte stores | Lightweight, WS-friendly |
| WebSocket client | Native WebSocket API | Direct connection to Hive server |
| Build tool | Vite | Fast HMR, Tauri-compatible |
| Desktop wrapper | Tauri v2 (Phase 2) | Same web frontend, native webview |

### Layout (three-panel, proven pattern)

```
+------------------+---------------------------+------------------+
|                  |                           |                  |
|   LEFT SIDEBAR   |      MAIN CONTENT         |  CONTEXT PANEL   |
|                  |                           |                  |
|  Workspace nav   |  Chat / Tasks / Agents    |  Details / Logs  |
|  Room list       |  (depends on view)        |  Agent info      |
|  Quick actions   |                           |  Task details    |
|                  |                           |                  |
+------------------+---------------------------+------------------+
```

**Top-level tabs:** Rooms | Agents | Tasks | Costs

## Views

### 1. Rooms View (default)
- Left: workspace tree with room list, unread badges
- Center: chat timeline for selected room (messages, system events, commands)
- Right: room info panel (members, statuses, recent activity)
- Real-time message streaming via WebSocket
- Inspired by: Slack channels, Discord servers

### 2. Agents View
- Center: card grid of all agents across workspace
- Each card shows: name, personality, model, uptime, health indicator (traffic light), current status, iteration count
- Click card: expand to show logs, cost breakdown, recent messages
- Actions: spawn (opens wizard), stop, restart
- Inspired by: Kubernetes Dashboard pods, Railway service cards

### 3. Tasks View
- Center: kanban board visualization of `/taskboard` data
- Columns: Open | Claimed | Planned | Approved | In Progress | Done
- Cards show: task ID, description, assignee, elapsed time, lease status
- Drag-and-drop assignment (sends `/taskboard assign` command)
- Inspired by: Linear, GitHub Projects

### 4. Costs View (Phase 2)
- Center: per-agent and per-team cost breakdown
- Charts: cost over time, cost by model, cost by agent
- Budget alerts and limits
- Inspired by: Cloud billing dashboards (AWS, GCP)

## Real-time patterns

| Pattern | Source | Implementation |
|---|---|---|
| Live presence | Discord green dots | Agent health indicators update via WS |
| Activity feed | GitHub notifications | System events stream in sidebar |
| Live status | Figma collaborator avatars | Agent status text updates in real-time |
| Unread counts | Slack channel badges | Per-room message counters |

## Implementation phases

### Phase 1: Web Dashboard MVP
- Project scaffolding (Vite + React/Svelte + Tailwind)
- WebSocket connection to Hive server
- Rooms view: room list, message timeline, member panel
- Basic agent list (read-only, from `/agent list` data)
- Login page (connects to Hive server OAuth)

### Phase 2: Interactive Features
- Agent spawn wizard (personality picker, model selector)
- Agent stop/restart from UI
- Task board kanban view
- Agent log viewer (streaming)

### Phase 3: Tauri Desktop
- Wrap web app in Tauri shell
- System tray icon with agent health summary
- Native notifications for @mentions and task assignments
- File system access for workspace management

### Phase 4: Advanced
- Cost dashboard with charts
- Cross-room unified timeline
- Agent discovery and assignment UI
- Team creation wizard

## Component inventory (estimated)

| Component | Description | Priority |
|---|---|---|
| `<AppShell>` | Three-panel layout with tab navigation | P0 |
| `<RoomList>` | Sidebar room tree with unread badges | P0 |
| `<ChatTimeline>` | Message list with auto-scroll | P0 |
| `<MemberPanel>` | Room members with status | P0 |
| `<AgentCard>` | Agent status card with health indicator | P0 |
| `<AgentGrid>` | Grid layout of agent cards | P0 |
| `<SpawnWizard>` | Agent spawn form (personality, model) | P1 |
| `<LogViewer>` | Streaming log viewer for agents | P1 |
| `<TaskBoard>` | Kanban board for taskboard data | P1 |
| `<TaskCard>` | Draggable task card | P1 |
| `<CostChart>` | Cost over time visualization | P2 |
| `<LoginPage>` | OAuth login flow | P0 |

## Resolved questions

1. **React or Svelte?** TBD — run a 1-day spike to compare DX. Both work with Tauri.
2. **Same frontend for web and desktop?** Yes — Tauri wraps the same web app. Zero rewrite. Per r2d2 investigation.
3. **Server-side rendering?** No — SPA with client-side WS. Hive server serves static assets.
4. **Design system?** Start with Tailwind + headless UI components. Custom design system later.
