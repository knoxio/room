# [FE-011] Task Board Kanban View with Columns

**As a** user
**I want to** see all tasks visualized as a kanban board with status columns
**So that** I can understand task progress across the team at a glance

## Acceptance Criteria
- [ ] The Tasks tab renders a `<TaskBoard>` component with columns: Open, Claimed, Planned, Approved, In Progress, Done
- [ ] Each column displays `<TaskCard>` components for tasks in that status, ordered by creation time (newest at top)
- [ ] Each `<TaskCard>` shows: task ID, description (truncated to 2 lines), assignee avatar/username (if claimed), elapsed time since creation, and lease status indicator (green = active lease, orange = lease expiring soon, red = lease expired)
- [ ] Column headers display the count of tasks in each column (e.g., "Open (3)")
- [ ] Task data is fetched from the Hive server's taskboard API on tab activation and updated in real-time via WebSocket events (TaskPosted, TaskClaimed, TaskFinished, etc.)
- [ ] Clicking a task card opens a detail view in the context panel showing: full description, assignee, implementation plan (if submitted), status history, and timestamps for each transition
- [ ] Cancelled tasks are excluded from the board by default but can be shown via a "Show cancelled" toggle
- [ ] The board handles empty columns gracefully with a subtle placeholder message

## Phase
Phase 2: Interactive Features

## Priority
P1

## Components
- TaskBoard
- TaskCard

## Notes
The kanban columns map to the taskboard lifecycle: Open -> Claimed -> Planned -> Approved -> Finished. "In Progress" maps to the Approved status (agent is actively working). The EventType enum in room-protocol includes TaskPosted, TaskClaimed, etc. for real-time updates. Drag-and-drop interaction is covered separately in FE-012.
