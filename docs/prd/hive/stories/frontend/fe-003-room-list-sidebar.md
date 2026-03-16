# [FE-003] Room List Sidebar with Workspace Grouping

**As a** user
**I want to** see all rooms in the left sidebar grouped by workspace
**So that** I can quickly navigate between rooms and find the one I need

## Acceptance Criteria
- [ ] The `<RoomList>` component renders inside the left sidebar panel and fetches the list of rooms from the Hive server API
- [ ] Rooms are grouped under collapsible workspace headings; each workspace section can be expanded or collapsed independently
- [ ] Clicking a room name selects it and loads its chat timeline in the main content area
- [ ] The currently selected room is visually highlighted with a distinct background color
- [ ] Each room entry displays the room name and a truncated preview of the last message (sender + first ~40 characters)
- [ ] Rooms within a workspace are sorted by most-recent activity (most recent at top)
- [ ] A search/filter input at the top of the sidebar filters rooms by name in real-time (client-side filtering)
- [ ] Empty state: when no rooms exist, a helpful message is displayed with a link to create a room (if authorized)

## Phase
Phase 1: Web Dashboard MVP

## Priority
P0

## Components
- RoomList

## Notes
Workspace grouping follows the hierarchy defined in prd-workspace.md. The room list updates in real-time as new rooms are created or destroyed (via WebSocket events from the Hive server). Unread badges are deferred to FE-013.
