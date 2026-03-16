# US-BE-008: GitHub OAuth login flow

**As a** human user
**I want to** log in to Hive using my GitHub account
**So that** I have a persistent, authenticated identity without managing a separate password

## Acceptance Criteria
- [ ] `GET /auth/github/login` redirects the browser to GitHub's OAuth authorization URL with the correct `client_id`, `scope` (`read:user user:email`), and a CSRF `state` parameter
- [ ] `GET /auth/github/callback?code=<code>&state=<state>` exchanges the code for an access token, fetches the GitHub user profile, and creates or updates the Hive `users` record
- [ ] CSRF `state` is validated on callback; mismatched state returns `400 Bad Request`
- [ ] On success, callback redirects to the frontend with a short-lived exchange token in the query string; frontend exchanges it for a JWT via `POST /auth/token`
- [ ] GitHub `client_id` and `client_secret` are read from config (`hive.toml` or env vars `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET`)
- [ ] If GitHub is unreachable, returns `502 Bad Gateway` with a user-facing error message
- [ ] Existing users are matched by GitHub user ID (not email); email is stored but not used for identity

## Technical Notes
- Implement in `crates/hive-server/src/auth.rs`
- Use `oxide-auth` or direct `reqwest` calls to the GitHub OAuth endpoints; prefer direct calls to avoid pulling in a large dependency for a two-step flow
- CSRF state is a cryptographically random UUID stored in a short-lived (5 min TTL) in-memory map keyed by state; no session cookie needed at this stage
- GitHub API endpoints: `https://github.com/login/oauth/authorize`, `https://github.com/login/oauth/access_token`, `https://api.github.com/user`
- User record schema: see `users` table in US-BE-023

## Phase
Phase 2 (Auth + Agent Management)
