# US-BE-007: Room send proxy

**As a** Hive frontend user
**I want to** send a message to a room via the Hive server
**So that** my messages are delivered to the room daemon and broadcast to all participants

## Acceptance Criteria
- [ ] `POST /api/rooms/:id/send` with JSON body `{"content": "..."}` forwards the message to the room daemon and returns `200 OK` with the echoed message envelope
- [ ] Returns `400 Bad Request` with `{"error": "missing_content"}` if `content` is absent or empty
- [ ] Returns `404 Not Found` with `{"error": "room_not_found"}` if the room does not exist on the daemon
- [ ] Returns `502 Bad Gateway` if the daemon is unreachable
- [ ] In Phase 1 (no auth), uses a configurable service account token from `hive.toml` (`service_token`) for the daemon `Authorization` header
- [ ] Request body size is limited to 64 KB (matching room daemon's `MAX_LINE_BYTES`)

## Technical Notes
- Implement in `crates/hive-server/src/rooms.rs`
- Calls daemon `POST /api/<room_id>/send` with `Authorization: Bearer <service_token>`
- In Phase 2, the service_token is replaced by the authenticated user's room token (from token mapping, US-BE-011)
- Body size limit enforced via axum's `ContentLengthLimit` extractor or a custom middleware

## Phase
Phase 1 (Skeleton + Room Proxy)
