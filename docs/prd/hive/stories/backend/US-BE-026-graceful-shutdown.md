# US-BE-026: Graceful shutdown

**As a** platform operator
**I want to** shut down the Hive server cleanly on SIGTERM or SIGINT
**So that** in-flight requests complete, agents receive stop signals, and WebSocket connections are closed without data loss

## Acceptance Criteria
- [ ] On SIGTERM or SIGINT, the server stops accepting new connections immediately
- [ ] In-flight HTTP requests are given up to 30 seconds to complete before the process exits; configurable via `shutdown_timeout_secs` in `hive.toml`
- [ ] All active WebSocket relay sessions (US-BE-003) receive a Close frame before the server exits
- [ ] All running agents (US-BE-012) receive SIGTERM on server shutdown; the server waits up to `agent_stop_timeout_secs` (default 10s) for them to exit before sending SIGKILL
- [ ] SQLite WAL is checkpointed before process exit to ensure data is not lost
- [ ] A log line at `INFO` level is emitted at shutdown start, and at `INFO` level again when shutdown is complete
- [ ] If the shutdown timeout is exceeded, the process exits with code 1; otherwise exits with code 0

## Technical Notes
- Implement in `crates/hive-server/src/main.rs`
- Use `tokio::signal::ctrl_c()` and `tokio::signal::unix::signal(SignalKind::terminate())` for signal handling; race them with `tokio::select!`
- Axum graceful shutdown: pass a shutdown future to `axum::serve(...).with_graceful_shutdown(shutdown_rx)`
- WebSocket close: broadcast a cancellation token to all relay tasks (stored in the `AppState`); each relay task catches the cancellation and sends a WS Close frame
- Agent shutdown: iterate the in-memory `AgentRegistry`, call the same SIGTERM + wait + SIGKILL sequence as US-BE-013
- SQLite checkpoint: call `PRAGMA wal_checkpoint(TRUNCATE)` on the connection before dropping it

## Phase
Cross-cutting
