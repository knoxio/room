# US-BE-020: Batch agent provisioning

**As a** workspace owner
**I want to** spawn all agents defined in a team manifest with a single API call
**So that** I can stand up an entire team quickly without calling the spawn endpoint N times

## Acceptance Criteria
- [ ] `POST /api/workspaces/:id/manifests/:mid/provision` spawns all agents in the manifest and returns `202 Accepted` with `{"job_id": "...", "agent_count": N, "status": "provisioning"}`
- [ ] `GET /api/workspaces/:id/provision/:job_id` returns the provisioning status: `{"status": "provisioning|done|partial_failure", "agents": [{"personality": "...", "agent_id": "...", "status": "starting|running|failed", "error": "..."}]}`
- [ ] Agents are spawned concurrently (not sequentially); all spawn attempts are made regardless of individual failures
- [ ] If any agent fails to spawn, the job status is `partial_failure`; already-started agents are NOT stopped automatically
- [ ] Provisioning respects the `max_agents_per_workspace` limit; if the total would exceed it, the request fails with `422` before any agents are spawned
- [ ] Provisioning is idempotent if called again with the same manifest: agents already running with the same personality+room are skipped (not duplicated)

## Technical Notes
- Implement in `crates/hive-server/src/agents.rs`
- Provisioning job state is tracked in SQLite: `provision_jobs` table with `id`, `workspace_id`, `manifest_id`, `status`, `created_at`; individual agent results in `provision_job_agents`
- Uses the same `provision_agent_workspace` + `spawn` logic from US-BE-012/US-BE-016
- `tokio::spawn` one task per agent; collect results via `futures::future::join_all`
- Job ID is a UUID; the `GET` poll endpoint is the only status mechanism (no SSE/WebSocket for provisioning in Phase 3)

## Phase
Phase 3 (Workspaces + Teams)
