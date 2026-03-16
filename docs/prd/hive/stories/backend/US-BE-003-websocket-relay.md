# US-BE-003: WebSocket relay

**As a** Hive frontend user
**I want to** connect to a room via the Hive server's WebSocket endpoint
**So that** I receive real-time messages from the room daemon without connecting to it directly

## Acceptance Criteria
- [ ] `GET /ws/:room_id` upgrades to a WebSocket connection and relays frames bidirectionally to the room daemon at `ws://<daemon_host>/ws/:room_id`
- [ ] Text frames from the frontend are forwarded to the daemon unchanged
- [ ] Text frames from the daemon are forwarded to the frontend unchanged
- [ ] Binary frames are forwarded unchanged
- [ ] When the daemon connection closes, the frontend connection is closed with the same close code
- [ ] When the frontend disconnects, the daemon connection is torn down cleanly
- [ ] Connection errors to the daemon return an HTTP `502 Bad Gateway` before the WebSocket upgrade completes
- [ ] Relay handles at least 100 concurrent connections without degradation

## Technical Notes
- Implement in `crates/hive-server/src/ws_relay.rs`
- Use `tokio-tungstenite` to open the upstream daemon WebSocket connection
- Use axum's `WebSocketUpgrade` extractor for the frontend-facing side
- Spawn two tasks per relay session: one for each direction; both tasks share a cancellation token so either side closing tears down the other
- Daemon WebSocket URL is derived from `config.room_ws_url` (e.g. `ws://127.0.0.1:4200/ws/<room_id>`)
- No message transformation in Phase 1; raw relay only

## Phase
Phase 1 (Skeleton + Room Proxy)
