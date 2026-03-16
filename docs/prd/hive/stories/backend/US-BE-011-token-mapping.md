# US-BE-011: Token mapping

**As a** Hive server
**I want to** mint a room token for each authenticated Hive user
**So that** messages sent through the Hive API are attributed to the correct room identity

## Acceptance Criteria
- [ ] When a user first authenticates via OAuth, the server calls `room join <username>` and persists the returned room token in the `users.room_token` column
- [ ] The room token is refreshed if the room daemon is restarted (detected by a `401` from the daemon on token use)
- [ ] All proxied `POST /api/rooms/:id/send` requests use the caller's room token, not the service account token
- [ ] Room username is derived from the user's GitHub login (truncated to 32 chars, lowercased, non-alphanumeric replaced with `-`)
- [ ] If `room join` fails, the OAuth login still succeeds but the user's room token is `NULL`; subsequent room actions return `503` with `{"error": "room_token_unavailable"}`
- [ ] Token refresh is retried once with exponential backoff before surfacing the error

## Technical Notes
- Implement in `crates/hive-server/src/auth.rs`
- `room join` is invoked via `tokio::process::Command` using the `room` binary found on `PATH` or at `config.room_binary_path`
- Parse the JSON output `{"type":"token","token":"<uuid>","username":"<name>"}` to extract the token
- Token is stored encrypted at rest using AES-128-GCM with a key derived from `HIVE_SECRET_KEY` env var; stored as base64 in the `users.room_token` column
- On each room API call, decrypt the token from the database, inject as `Authorization: Bearer <token>` when forwarding to the daemon

## Phase
Phase 2 (Auth + Agent Management)
