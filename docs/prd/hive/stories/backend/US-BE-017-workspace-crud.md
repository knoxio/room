# US-BE-017: Workspace CRUD

**As a** authenticated Hive user
**I want to** create, view, update, and delete workspaces
**So that** I can organise related rooms and agent teams into named project spaces

## Acceptance Criteria
- [ ] `POST /api/workspaces` with `{"name": "..."}` creates a workspace and returns `201 Created` with the full workspace object
- [ ] `GET /api/workspaces` returns all workspaces accessible to the authenticated user
- [ ] `GET /api/workspaces/:id` returns a single workspace or `404 Not Found`
- [ ] `PATCH /api/workspaces/:id` updates the workspace name; returns `200 OK` with the updated object
- [ ] `DELETE /api/workspaces/:id` deletes the workspace and cascades to `workspace_rooms` and `team_manifests`; does NOT destroy rooms on the daemon (rooms are independent)
- [ ] Workspace names must be 1–64 characters; duplicates per user return `409 Conflict`
- [ ] Only the workspace owner can update or delete it; others receive `403 Forbidden`

## Technical Notes
- Implement in `crates/hive-server/src/workspaces.rs` (new module)
- SQLite schema: see `workspaces` and `workspace_rooms` tables in US-BE-023
- Owner is set to `current_user.id` on creation and stored in `workspaces.owner_id`
- Cascade delete is handled by SQLite foreign key constraints (`ON DELETE CASCADE`) on `workspace_rooms` and `team_manifests`
- Foreign key enforcement must be enabled per connection: `PRAGMA foreign_keys = ON`

## Phase
Phase 3 (Workspaces + Teams)
