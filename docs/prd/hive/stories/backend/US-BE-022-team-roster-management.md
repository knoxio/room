# US-BE-022: Team roster management

**As a** workspace owner
**I want to** add and remove human users from a workspace team
**So that** I can control who has access to the workspace and its rooms

## Acceptance Criteria
- [ ] `POST /api/workspaces/:id/members` with `{"user_id": "..."}` adds the user to the workspace team; returns `201 Created`
- [ ] `DELETE /api/workspaces/:id/members/:user_id` removes the user; returns `204 No Content`
- [ ] `GET /api/workspaces/:id/members` returns the list of team members with `id`, `github_login`, `email`, `role` (`owner|member`), `joined_at`
- [ ] The workspace owner cannot remove themselves; attempting to do so returns `422 Unprocessable Entity` with `{"error": "cannot_remove_owner"}`
- [ ] Adding a user who is already a member returns `409 Conflict`
- [ ] Only the workspace owner can add or remove members; `403 Forbidden` otherwise
- [ ] Members (non-owners) can read workspace resources but cannot modify workspace settings, add rooms, or delete the workspace

## Technical Notes
- Implement in `crates/hive-server/src/workspaces.rs`
- `workspace_members` join table: `(workspace_id, user_id, role, joined_at)`; `role` is a TEXT column with values `owner` or `member`
- Owner is automatically added as a member with `role = 'owner'` on workspace creation
- Authorization check in the auth middleware: extract workspace membership from SQLite for every request matching `/api/workspaces/:id/*`; inject a `WorkspaceMembership` extension
- User lookup for `POST` uses `users.id`; non-existent users return `404 Not Found`

## Phase
Phase 3 (Workspaces + Teams)
