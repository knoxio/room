# US-BE-001: Server health endpoint

**As a** platform operator
**I want to** query a health endpoint on the Hive server
**So that** load balancers and monitoring systems can determine whether the server is running and ready to accept traffic

## Acceptance Criteria
- [ ] `GET /api/health` returns `200 OK` with a JSON body containing at least `{"status": "ok"}`
- [ ] Response includes server version and uptime in seconds
- [ ] Response includes room daemon connectivity status (`daemon_connected: true/false`)
- [ ] Endpoint is unauthenticated (no token required)
- [ ] Response time is under 100ms under normal load
- [ ] Returns `503 Service Unavailable` with `{"status": "degraded", "reason": "..."}` when the room daemon is unreachable

## Technical Notes
- Implement in `crates/hive-server/src/main.rs` or a dedicated `health.rs` handler
- Daemon connectivity check: attempt a short-timeout ping to the room daemon WebSocket or REST `GET /api/health`; do not block on it — use a cached status updated every 5s
- Use `axum::Router` to register the route
- Uptime is calculated from a `start_time: std::time::Instant` stored in shared `AppState`

## Phase
Phase 1 (Skeleton + Room Proxy)
