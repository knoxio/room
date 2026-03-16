# [FE-007] WebSocket Connection Management

**As a** user
**I want to** have a reliable WebSocket connection to the Hive server that handles disconnections gracefully
**So that** I receive real-time updates without manual intervention when network issues occur

## Acceptance Criteria
- [ ] The WebSocket client connects to the Hive server using the native WebSocket API on successful authentication, using the `SESSION:<token>` handshake protocol
- [ ] On unexpected disconnection, the client automatically attempts reconnection with exponential backoff (1s, 2s, 4s, 8s, capped at 30s) and a jitter factor to avoid thundering herd
- [ ] A connection status indicator is visible in the app shell (e.g., top bar): green = connected, yellow = reconnecting, red = disconnected
- [ ] During reconnection, a non-blocking banner informs the user ("Reconnecting...") without obscuring the UI
- [ ] After successful reconnection, the client fetches missed messages via the REST poll endpoint (using the last-seen message ID as cursor) and merges them into the timeline without duplicates
- [ ] WebSocket errors (invalid token, server rejection, protocol errors) are logged to the browser console and surfaced to the user with actionable messages (e.g., "Session expired, please log in again")
- [ ] The connection is cleanly closed on logout or page unload (sends proper WebSocket close frame)
- [ ] Heartbeat/ping frames are sent at a configurable interval (default 30s) to detect dead connections before the TCP timeout

## Phase
Phase 1: Web Dashboard MVP

## Priority
P0

## Components
- AppShell (connection indicator)

## Notes
This is a cross-cutting concern that underpins all real-time features. The WebSocket connection state should live in the global store (Zustand/Svelte store) so any component can read connection status. The handshake protocol is documented in CLAUDE.md under "WebSocket endpoint". The `SESSION:<token>` handshake validates the token and enters interactive mode. Reconnection must re-authenticate using the stored token.
