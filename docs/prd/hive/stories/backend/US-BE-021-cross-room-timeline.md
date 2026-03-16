# US-BE-021: Cross-room timeline

**As a** workspace owner
**I want to** fetch a unified timeline of messages from all rooms in a workspace
**So that** I can see a chronological view of all activity without switching between rooms

## Acceptance Criteria
- [ ] `GET /api/workspaces/:id/timeline` returns a JSON array of messages from all rooms in the workspace, sorted by `ts` ascending
- [ ] Each message includes a `room_id` field identifying its source room
- [ ] Supports `?limit=N` (default 100, max 1000) and `?before=<ts>` (ISO 8601) cursor for pagination
- [ ] If a room in the workspace is unavailable, its messages are omitted and a `"warnings": [{"room_id": "...", "error": "..."}]` field is included in the response
- [ ] Empty workspaces (no rooms) return `{"messages": [], "warnings": []}`
- [ ] Returns `404 Not Found` if the workspace does not exist or the caller does not have access

## Technical Notes
- Implement in `crates/hive-server/src/workspaces.rs`
- Fetch message history from each room in parallel using `tokio::spawn` + `join_all`; each fetch calls the daemon's history endpoint (US-BE-006 logic re-used)
- Merge step: collect all messages into a `Vec<MessageSummary>`, sort by `ts` (parse as `DateTime<Utc>`), apply `limit` after sort
- `before=<ts>` filtering: after the sort, drop all messages with `ts >= before`, then take the last `limit` entries (equivalent to "messages before this timestamp")
- The total fetch per room is capped at `limit * room_count` to avoid fetching unbounded history; this is a best-effort approximation

## Phase
Phase 3 (Workspaces + Teams)
