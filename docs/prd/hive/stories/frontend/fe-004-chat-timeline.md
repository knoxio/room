# [FE-004] Chat Timeline with Real-Time Message Streaming via WebSocket

**As a** user
**I want to** see a real-time chat timeline for the selected room
**So that** I can follow conversations and system events as they happen

## Acceptance Criteria
- [ ] The `<ChatTimeline>` component renders all message types from the room wire format: `message`, `join`, `leave`, `system`, `command`, `reply`, `direct_message`, and `event`
- [ ] New messages arriving via WebSocket are appended to the timeline instantly without requiring a manual refresh
- [ ] The timeline auto-scrolls to the latest message when the user is already at the bottom; if the user has scrolled up, a "new messages" pill appears at the bottom instead of force-scrolling
- [ ] Each message displays: sender username, timestamp (relative, e.g., "2m ago"), and content; system messages and events use a distinct visual style (muted, no avatar)
- [ ] @mentions of the current user are highlighted within message text
- [ ] On initial room selection, the timeline loads the most recent N messages (history backfill from the REST poll endpoint) and supports upward scroll to load older messages (pagination)
- [ ] Direct messages are visually distinct (e.g., italicized, with a DM badge) and only shown to the sender and recipient
- [ ] Reply messages display a quoted excerpt of the parent message above the reply content

## Phase
Phase 1: Web Dashboard MVP

## Priority
P0

## Components
- ChatTimeline

## Notes
The wire format is defined in room-protocol (see CLAUDE.md codebase overview). The WebSocket connection is managed by FE-007. History backfill uses the REST `/api/<room_id>/poll` endpoint. Message rendering should handle markdown-like formatting if the content contains code blocks or inline code.
