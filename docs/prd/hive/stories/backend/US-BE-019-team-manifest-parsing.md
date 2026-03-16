# US-BE-019: Team manifest parsing

**As a** workspace owner
**I want to** upload a JSON team manifest that describes a set of agents
**So that** I can define a repeatable team configuration and provision it on demand

## Acceptance Criteria
- [ ] `POST /api/workspaces/:id/manifests` with a JSON body conforming to the manifest schema stores the manifest and returns `201 Created` with `{"manifest_id": "...", "agent_count": N}`
- [ ] `GET /api/workspaces/:id/manifests` returns all manifests for the workspace
- [ ] `GET /api/workspaces/:id/manifests/:mid` returns a single manifest
- [ ] `DELETE /api/workspaces/:id/manifests/:mid` removes the manifest (does not affect running agents)
- [ ] Malformed JSON returns `400 Bad Request` with field-level validation errors
- [ ] Manifest schema: `{"name": "...", "agents": [{"personality": "...", "model": "...", "room_id": "..."}]}`; all fields required per agent entry
- [ ] Maximum 50 agents per manifest; exceeding this returns `422 Unprocessable Entity`

## Technical Notes
- Implement in `crates/hive-server/src/workspaces.rs`
- Manifest body is validated into a `TeamManifest` struct before storage; store the validated JSON (re-serialised from the struct) in `team_manifests.manifest_json`
- Validation uses serde's derive macros with `#[serde(deny_unknown_fields)]` to reject unrecognised fields
- `agent_count` in the response is the length of the `agents` array
- `room_id` values in the manifest are validated against the daemon's room list at upload time (same approach as US-BE-018)

## Phase
Phase 3 (Workspaces + Teams)
