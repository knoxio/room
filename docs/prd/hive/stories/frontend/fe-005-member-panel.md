# [FE-005] Member Panel Showing Room Participants with Status

**As a** user
**I want to** see a list of all participants in the current room with their live status
**So that** I can understand who is present, what they are working on, and whether agents are healthy

## Acceptance Criteria
- [ ] The `<MemberPanel>` component renders in the right context panel when the Rooms view is active
- [ ] All current room members are listed, each showing: username, online/offline indicator, and current status text (from `/set_status`)
- [ ] Members are sorted into two groups: "Online" (connected) and "Offline" (disconnected but previously joined), with online members listed first
- [ ] Agent members display a bot icon to distinguish them from human users
- [ ] Status text updates in real-time as agents and users change their status via WebSocket events
- [ ] Online presence indicators (green dot = connected, gray dot = disconnected) update within 5 seconds of a join/leave event
- [ ] Clicking a member name opens a mini-profile popover with: username, current status, time since last message, and a "Send DM" action
- [ ] The panel displays a member count header (e.g., "Members (12)") that updates dynamically

## Phase
Phase 1: Web Dashboard MVP

## Priority
P0

## Components
- MemberPanel

## Notes
Presence data comes from WebSocket join/leave events and the `/who` command response. Agent health indicators (traffic light) are part of the Agents view (FE-006) and are not duplicated here, but the online/offline dot serves as a lightweight proxy. Status text corresponds to the `/set_status` slash command output.
