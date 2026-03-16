# US-BE-014: Agent list

**As a** workspace owner
**I want to** see all agents with their current health, status, uptime, and model
**So that** I can monitor the health of my team and identify stuck or crashed agents

## Acceptance Criteria
- [ ] `GET /api/agents` returns a JSON array of agent objects for all agents the caller has access to
- [ ] `GET /api/agents?workspace_id=<id>` filters agents by workspace
- [ ] Each agent object includes: `id`, `room_id`, `personality`, `model`, `status` (`starting|running|stopped|crashed`), `pid`, `uptime_secs`, `workspace_id`, `started_at`, `stopped_at`
- [ ] `status` reflects the live process state: `running` if the PID is alive, `crashed` if the process exited with non-zero code, `stopped` if cleanly terminated
- [ ] `GET /api/agents/:id` returns a single agent or `404 Not Found`
- [ ] Response is returned within 200ms; process liveness check uses cached state updated by the background monitor (not a live OS query per request)

## Technical Notes
- Implement in `crates/hive-server/src/agents.rs`
- Agent status is maintained by the background monitor task (see US-BE-012) which polls `child.try_wait()` every 5 seconds and updates both the in-memory `AgentHandle` and the SQLite record
- On server restart, agents that were `running` at shutdown are marked `stopped` (processes are orphaned and not re-adopted); operators must re-spawn them
- Response serialization uses a `AgentSummary` DTO distinct from the internal `AgentHandle` to avoid exposing the `Child` handle

## Phase
Phase 2 (Auth + Agent Management)
