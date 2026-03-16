# [FE-017] Native Notifications for @mentions and Task Assignments

**As a** desktop user
**I want to** receive native OS notifications when I am @mentioned or assigned a task
**So that** I can respond promptly without keeping the Hive window in focus

## Acceptance Criteria
- [ ] When a message containing an @mention of the current user arrives via WebSocket and the Hive window is not focused, a native OS notification is displayed with: sender name, room name, and a truncated message preview (first 100 characters)
- [ ] When a task is assigned to the current user (TaskClaimed event with matching username), a native notification is displayed with: "Task assigned: <task-id> - <description truncated>"
- [ ] Clicking a notification brings the Hive window to the foreground and navigates to the relevant room (for @mentions) or the Tasks view (for task assignments)
- [ ] Notification preferences are configurable: users can independently toggle @mention notifications, task assignment notifications, and DM notifications on or off
- [ ] A "Do Not Disturb" mode suppresses all notifications; it can be toggled from the system tray menu or the app preferences
- [ ] Notifications respect the OS-level notification settings (e.g., macOS Focus modes, Windows Quiet Hours) -- the app does not attempt to bypass system-level suppression
- [ ] Notification sound follows the OS default; no custom sounds are bundled in Phase 3
- [ ] Rate limiting: if more than 5 notifications arrive within 10 seconds, they are batched into a single summary notification ("5 new messages in #room-name")

## Phase
Phase 3: Tauri Desktop

## Priority
P1

## Components
- AppShell (Tauri integration)

## Notes
Tauri v2 provides native notification APIs via the `notification` plugin. On macOS, the app must request notification permissions on first launch. The web version can use the browser Notification API as a fallback, but this story focuses on the Tauri desktop experience. Rate limiting prevents notification storms during high-activity sprints.
