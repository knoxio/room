# US-BE-012: Agent spawn

**As a** workspace owner
**I want to** spawn an AI agent with a specific personality, model, and room assignment
**So that** the agent joins the room and begins working autonomously

## Acceptance Criteria
- [ ] `POST /api/agents` with body `{"room_id": "...", "personality": "...", "model": "sonnet|opus|haiku", "workspace_id": "..."}` spawns a `room-ralph` process and returns `{"agent_id": "<uuid>", "status": "starting", "pid": N}`
- [ ] The spawned `room-ralph` process has its own working directory under `config.data_dir/agents/<agent_id>/`
- [ ] `personality` is a string passed as `--personality` to `room-ralph`; invalid personalities return `400 Bad Request`
- [ ] `model` defaults to `sonnet` if absent
- [ ] Agent record is created in SQLite with status `starting`; status transitions to `running` once the process is confirmed alive
- [ ] Returns `409 Conflict` if an agent with the same `personality` is already running in the same room
- [ ] Maximum concurrent agents per workspace is enforced (`max_agents_per_workspace` in config, default 10)

## Technical Notes
- Implement in `crates/hive-server/src/agents.rs`
- Use `tokio::process::Command` to spawn `room-ralph`; store the `Child` handle in an in-memory `AgentRegistry` (`DashMap<Uuid, AgentHandle>`)
- `AgentHandle` stores: `pid`, `room_id`, `personality`, `model`, `start_time`, `log_file_path`, `child: tokio::process::Child`
- `room-ralph` is found via `config.ralph_binary_path` or `PATH`
- Stdout/stderr of the child process is redirected to `config.data_dir/agents/<agent_id>/agent.log`
- A background task monitors the child's exit status and updates the SQLite status to `stopped` on exit

## Phase
Phase 2 (Auth + Agent Management)
