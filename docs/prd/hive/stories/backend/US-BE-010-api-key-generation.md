# US-BE-010: API key generation

**As a** authenticated Hive user
**I want to** create and revoke API keys
**So that** I can give programmatic access to scripts and CI pipelines without sharing my OAuth session

## Acceptance Criteria
- [ ] `POST /api/keys` (authenticated) creates an API key and returns `{"key": "hive_<random>", "id": "<uuid>", "scopes": [...], "created_at": "..."}` — the raw key is shown only once
- [ ] `GET /api/keys` returns all active keys for the authenticated user (IDs and metadata only, not raw keys)
- [ ] `DELETE /api/keys/:id` revokes the key immediately; subsequent requests using it receive `401 Unauthorized`
- [ ] Keys are accepted in the `Authorization: Bearer hive_<value>` header on all authenticated endpoints
- [ ] Scopes are validated at creation time; unsupported scopes return `400 Bad Request`
- [ ] Initial supported scopes: `rooms:read`, `rooms:write`, `agents:read`, `agents:write`
- [ ] A key belongs to exactly one user; permissions are the intersection of the user's permissions and the key's scopes

## Technical Notes
- Implement in `crates/hive-server/src/auth.rs`
- Raw key format: `hive_` prefix + 32 bytes of random data encoded as base64url (total ~48 chars)
- Only the SHA-256 hash of the raw key is stored in the `api_keys` table (see US-BE-023)
- Auth middleware checks the `Authorization` header prefix: `hive_` routes to API key validation, all other Bearer values are treated as JWTs
- Scopes stored as a JSON array in the `api_keys.scopes` column

## Phase
Phase 2 (Auth + Agent Management)
