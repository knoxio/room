# US-BE-006: Room messages endpoint

**As a** Hive frontend user
**I want to** fetch the message history for a room
**So that** I can display past messages when opening a room channel

## Acceptance Criteria
- [ ] `GET /api/rooms/:id/messages` returns `200 OK` with a JSON array of message objects in chronological order
- [ ] Supports pagination via `?limit=N` (default 50, max 500) and `?before=<message_id>` cursor
- [ ] Each message object includes: `id`, `room`, `user`, `content`, `ts`, `type`
- [ ] Returns `404 Not Found` with `{"error": "room_not_found"}` if the room does not exist
- [ ] Returns `200 OK` with `[]` if the room exists but has no messages
- [ ] If the daemon is unavailable, returns `502 Bad Gateway`

## Technical Notes
- Implement in `crates/hive-server/src/rooms.rs`
- Delegates to daemon `GET /api/<room_id>/poll?limit=N&since=<id>` or equivalent history endpoint
- Response schema uses a `MessageSummary` struct; fields map directly from `room_protocol::Message` variants but are flattened to a uniform shape for the frontend (all types included, `content` is `Option<String>`)
- Message ID is the `id` field from the room-protocol envelope (UUID string)
- `before` cursor translates to `?since=` with a reversed scan if the daemon supports it; otherwise fetch last N and filter

## Phase
Phase 1 (Skeleton + Room Proxy)
