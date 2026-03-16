# US-BE-004: REST room proxy

**As a** Hive frontend developer
**I want to** make REST calls to `/api/rooms/*` on the Hive server
**So that** the frontend does not need to know the room daemon's address or port

## Acceptance Criteria
- [ ] All requests to `/api/rooms/*` are forwarded to the room daemon's corresponding REST endpoint
- [ ] Request method, headers, and body are preserved on the forwarded request
- [ ] Response status code, headers, and body from the daemon are returned to the caller unchanged
- [ ] A `X-Hive-Request-ID` header is injected into every proxied request for traceability
- [ ] If the daemon returns a non-2xx response, it is passed through to the caller without modification
- [ ] If the daemon is unreachable, the proxy returns `502 Bad Gateway` with `{"error": "daemon_unavailable"}`
- [ ] Proxy timeout is configurable (`proxy_timeout_secs` in `hive.toml`, default 30s)

## Technical Notes
- Implement in `crates/hive-server/src/rooms.rs` using `reqwest` as the HTTP client
- Route pattern: `any("/api/rooms/*path", proxy_handler)` — strip the `/api/rooms` prefix and rewrite to the daemon base URL
- Daemon base URL stored in `AppState` as `daemon_rest_url` derived from config
- Re-use a single `reqwest::Client` stored in `AppState` (connection pooling)
- Phase 1 only: no auth injection. Auth headers added in Phase 2

## Phase
Phase 1 (Skeleton + Room Proxy)
