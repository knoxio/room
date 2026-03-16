# [FE-013] Unread Message Badges per Room

**As a** user
**I want to** see unread message counts on each room in the sidebar
**So that** I know which rooms have new activity without checking each one individually

## Acceptance Criteria
- [ ] Each room entry in the `<RoomList>` displays an unread badge (numeric count) when there are messages the user has not seen
- [ ] The unread count increments in real-time as new messages arrive via WebSocket for rooms the user is not currently viewing
- [ ] Selecting a room resets its unread count to zero and updates the read cursor (last-seen message ID) in the client store
- [ ] Badges use a compact format: exact count for 1-99, "99+" for higher counts
- [ ] Rooms with unread messages are visually promoted: bold room name and the badge uses an accent color (e.g., blue pill)
- [ ] @mentions of the current user in unread messages trigger a distinct badge style (e.g., red instead of blue) to indicate direct attention needed
- [ ] The total unread count across all rooms is displayed in the browser tab title (e.g., "(5) Hive") and updates dynamically
- [ ] Unread state persists across page reloads by storing the per-room read cursor in localStorage

## Phase
Phase 2: Interactive Features

## Priority
P1

## Components
- RoomList

## Notes
The read cursor per room tracks the last message ID the user has seen. This is purely client-side state in Phase 1/2; server-side read receipts could be added later. The badge rendering should match the "Slack channel badges" pattern referenced in the PRD's real-time patterns table.
