# [FE-019] Cross-Room Unified Timeline

**As a** user
**I want to** see a single chronological feed of messages from all my subscribed rooms
**So that** I can monitor activity across the workspace without switching between rooms

## Acceptance Criteria
- [ ] A "Unified" entry appears at the top of the `<RoomList>` sidebar (above workspace groups) that, when selected, renders a merged timeline in the main content area
- [ ] The unified timeline interleaves messages from all subscribed rooms in chronological order, with each message prefixed by a colored room label chip (e.g., "[sprint-14]", "[general]")
- [ ] Room label colors are deterministic per room (consistent hash to color) so users can visually distinguish rooms at a glance
- [ ] Clicking a room label chip on any message navigates to that specific room's timeline, scrolled to the clicked message
- [ ] The unified timeline supports the same real-time streaming as individual room timelines: new messages appear instantly via WebSocket
- [ ] A room filter dropdown above the timeline allows selecting a subset of rooms to include in the unified view (default: all subscribed)
- [ ] The unified timeline respects DM visibility: direct messages are only shown to the sender and recipient, same as in individual room views
- [ ] Performance: the unified timeline virtualizes rendering to handle high message volumes across many rooms without degrading scroll performance

## Phase
Phase 4: Advanced

## Priority
P2

## Components
- ChatTimeline
- RoomList

## Notes
This maps to the "multi-room workspace view with unified timeline" goal stated in the PRD. The existing `cmd_poll_multi` (oneshot/poll.rs) already merges messages from multiple rooms by timestamp -- the frontend implementation mirrors this logic client-side. The room filter state should be persisted in localStorage.
