# US-BE-009: Session management

**As a** authenticated Hive user
**I want to** receive a JWT after login and use it for subsequent API requests
**So that** I do not have to re-authenticate on every request

## Acceptance Criteria
- [ ] `POST /auth/token` with a valid exchange token returns `{"token": "<jwt>", "expires_in": 86400}` and invalidates the exchange token
- [ ] The JWT encodes `user_id`, `github_id`, `email`, and `exp` (24h expiry by default)
- [ ] All authenticated API endpoints accept `Authorization: Bearer <jwt>` and return `401 Unauthorized` with `{"error": "invalid_token"}` when the token is absent, expired, or malformed
- [ ] Token expiry is configurable (`jwt_expiry_secs` in `hive.toml`, default 86400)
- [ ] A `POST /auth/refresh` endpoint issues a new JWT given a valid, non-expired token (sliding window)
- [ ] `POST /auth/logout` invalidates the token by adding its `jti` to an in-memory revocation set (cleared on server restart)

## Technical Notes
- Implement in `crates/hive-server/src/auth.rs`
- Use `jsonwebtoken` crate for encoding/decoding with HS256; signing secret read from `HIVE_JWT_SECRET` env var (required, no default — server fails to start if absent)
- Claims struct: `{ sub: user_id, github_id, email, exp, jti }`
- Auth middleware implemented as an axum `Extension` extractor that validates the Bearer token and injects a `CurrentUser` struct into the handler
- Exchange tokens are single-use UUIDs stored in a `DashMap<String, (user_id, Instant)>` with a 5-minute TTL

## Phase
Phase 2 (Auth + Agent Management)
