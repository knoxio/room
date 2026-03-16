# US-BE-015: Agent logs

**As a** workspace owner
**I want to** view or stream the log output of a running or stopped agent
**So that** I can diagnose errors and observe agent behaviour

## Acceptance Criteria
- [ ] `GET /api/agents/:id/logs` returns the last 200 lines of the agent's log file as plain text
- [ ] `?lines=N` query parameter changes the number of lines returned (max 5000)
- [ ] `GET /api/agents/:id/logs?stream=true` upgrades to a Server-Sent Events (SSE) stream; new log lines are pushed to the client as they are written
- [ ] SSE stream closes automatically when the agent stops
- [ ] Returns `404 Not Found` if the agent does not exist
- [ ] Returns `204 No Content` if the agent exists but has produced no log output yet
- [ ] Log lines are returned as UTF-8; non-UTF-8 bytes are replaced with the Unicode replacement character

## Technical Notes
- Implement in `crates/hive-server/src/agents.rs`
- Log file path is `config.data_dir/agents/<agent_id>/agent.log`
- Tail implementation: read from end of file using `std::fs::File::seek(SeekFrom::End(-N_bytes))` — scan backwards for `\n` to find the Nth-from-last line; use a 64 KB scan window
- SSE streaming: use `tokio::io::BufReader` + `lines()` on the log file, then `tokio_stream::wrappers::LinesStream`; use axum's `Sse` response type
- For SSE, poll the file for new content using `tokio::time::interval` (100ms) rather than `inotify` to keep the implementation portable

## Phase
Phase 2 (Auth + Agent Management)
