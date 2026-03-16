# [FE-009] Agent Stop/Restart from UI

**As a** workspace admin
**I want to** stop or restart agents directly from the dashboard
**So that** I can manage agent lifecycle without switching to the terminal

## Acceptance Criteria
- [ ] Each `<AgentCard>` in the Agents view displays a context menu (kebab icon or right-click) with "Stop" and "Restart" actions
- [ ] The "Stop" action shows a confirmation dialog ("Stop agent X? This will terminate its current task.") before sending the stop command to the Hive server
- [ ] The "Restart" action shows a confirmation dialog and sends a restart command; the card transitions through "Stopping..." then "Starting..." states with visual feedback
- [ ] While an action is in progress, the corresponding menu items are disabled and the card shows a spinner overlay to prevent duplicate commands
- [ ] After a successful stop, the agent card transitions to a "Stopped" state (gray, distinct from error red) and the grid repositions it to the end
- [ ] After a successful restart, the agent card returns to healthy state once the first heartbeat arrives; if the agent fails to start within 60 seconds, the card shows an error state with the failure reason
- [ ] Bulk actions: a "Select All" checkbox enables stopping or restarting multiple agents at once via a toolbar action
- [ ] Stop/restart actions are only available to users with admin permissions; non-admin users see the actions as disabled with a tooltip explaining the restriction

## Phase
Phase 2: Interactive Features

## Priority
P1

## Components
- AgentCard
- AgentGrid

## Notes
Stop and restart map to Hive server API endpoints that manage agent processes. The agent process lifecycle is managed by room-ralph on the backend. The UI should optimistically update the card state and reconcile with the actual state from WebSocket events.
