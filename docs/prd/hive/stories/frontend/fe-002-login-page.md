# [FE-002] Login Page with Connection to Hive Server

**As a** user
**I want to** authenticate with the Hive server through a login page
**So that** I can access my workspaces and rooms securely

## Acceptance Criteria
- [ ] The `<LoginPage>` component renders a login form with server URL input and OAuth login button
- [ ] Unauthenticated users are redirected to the login page; authenticated users are redirected away from it
- [ ] OAuth flow opens the provider's consent screen, handles the callback, and stores the resulting token securely (httpOnly cookie or in-memory; not plain localStorage)
- [ ] The login page displays clear error messages for connection failures (server unreachable, invalid credentials, expired token)
- [ ] A loading spinner is shown during the OAuth handshake and token exchange
- [ ] After successful login, the user is redirected to the Rooms view (default landing)
- [ ] A "Remember server URL" option persists the last-used server address for faster reconnection
- [ ] Logout clears the stored token and returns the user to the login page

## Phase
Phase 1: Web Dashboard MVP

## Priority
P0

## Components
- LoginPage

## Notes
The PRD specifies OAuth login to the Hive server. The exact OAuth provider (GitHub, custom) depends on the Hive Server PRD. The frontend must handle the redirect-based OAuth flow and token refresh. Server URL input is necessary because Hive is self-hosted.
