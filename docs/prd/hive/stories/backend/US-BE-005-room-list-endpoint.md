# US-BE-005: Room list endpoint

**As a** Hive frontend user
**I want to** fetch a list of available rooms
**So that** I can display them in the workspace sidebar and allow users to join them

## Acceptance Criteria
- [ ] `GET /api/rooms` returns `200 OK` with a JSON array of room objects
- [ ] Each room object includes at minimum: `id`, `name`, `user_count`, `created_at`
- [ ] The list is sourced from the room daemon's room list API
- [ ] An empty list `[]` is returned (not an error) when no rooms exist
- [ ] Response is returned within 500ms under normal load
- [ ] If the daemon is unavailable, returns `502 Bad Gateway` with a structured error

## Technical Notes
- Implement in `crates/hive-server/src/rooms.rs`
- Calls daemon `GET /api/rooms` (or equivalent discovery endpoint) and re-serializes the result into Hive's schema
- Room list may be cached for up to 2 seconds to reduce daemon load under high frontend polling; cache invalidated on room create/destroy events
- Response schema is defined in a shared `RoomSummary` struct in `rooms.rs`; keep it decoupled from room-protocol types to allow independent evolution

## Phase
Phase 1 (Skeleton + Room Proxy)
