# US-BE-013: Agent stop

**As a** workspace owner
**I want to** stop a running agent
**So that** I can free resources or replace it with a different configuration

## Acceptance Criteria
- [ ] `DELETE /api/agents/:id` sends `SIGTERM` to the agent's process group and returns `202 Accepted` immediately
- [ ] If the process has not exited within 10 seconds, `SIGKILL` is sent to the process group
- [ ] After termination, the agent's SQLite record status is updated to `stopped` with a `stopped_at` timestamp
- [ ] Returns `404 Not Found` if the agent ID does not exist
- [ ] Returns `409 Conflict` if the agent is already stopped
- [ ] A `?force=true` query parameter skips the graceful period and sends `SIGKILL` immediately
- [ ] The agent's working directory and log file are retained for post-mortem inspection

## Technical Notes
- Implement in `crates/hive-server/src/agents.rs`
- Use `nix::sys::signal::killpg` (or `libc::killpg`) to signal the entire process group; the `room-ralph` process must be started with `process::Command::new(...).process_group(0)` so it leads its own group
- Timeout enforced with `tokio::time::timeout` wrapping `child.wait()`
- The `AgentHandle` is removed from the in-memory registry after the process exits
- Log file path is preserved in SQLite even after the handle is dropped

## Phase
Phase 2 (Auth + Agent Management)
