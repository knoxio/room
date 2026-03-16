# US-BE-018: Workspace room membership

**As a** workspace owner
**I want to** add and remove rooms from a workspace
**So that** I can define which rooms are part of a project and surface them in the workspace view

## Acceptance Criteria
- [ ] `POST /api/workspaces/:id/rooms` with `{"room_id": "..."}` adds the room to the workspace; returns `201 Created` with `{"workspace_id": "...", "room_id": "..."}`
- [ ] `DELETE /api/workspaces/:id/rooms/:room_id` removes the room from the workspace; returns `204 No Content`
- [ ] `GET /api/workspaces/:id/rooms` returns the list of rooms in the workspace, each with live metadata fetched from the daemon (user count, last message timestamp)
- [ ] Adding a room that does not exist on the daemon returns `422 Unprocessable Entity` with `{"error": "room_not_found_on_daemon"}`
- [ ] Adding a room already in the workspace returns `409 Conflict`
- [ ] Removing a room that is not in the workspace returns `404 Not Found`
- [ ] Only the workspace owner can modify room membership; others receive `403 Forbidden`

## Technical Notes
- Implement in `crates/hive-server/src/workspaces.rs`
- `workspace_rooms` is a join table: `(workspace_id, room_id)` with a unique constraint
- Room existence validation: call daemon `GET /api/rooms` (or `GET /api/<room_id>/health`) before inserting; cache the room list for 2 seconds to avoid hammering the daemon on bulk adds
- Live metadata for `GET /api/workspaces/:id/rooms` is fetched in parallel for all rooms using `tokio::join_all`; individual daemon failures return `null` metadata for that room rather than failing the whole request

## Phase
Phase 3 (Workspaces + Teams)
