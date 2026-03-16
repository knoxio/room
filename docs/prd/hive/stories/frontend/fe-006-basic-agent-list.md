# [FE-006] Basic Agent List (Read-Only)

**As a** user
**I want to** see a grid of all agents across the workspace with their current status
**So that** I can monitor agent health and activity at a glance

## Acceptance Criteria
- [ ] The Agents tab renders an `<AgentGrid>` component containing `<AgentCard>` components for each agent
- [ ] Each `<AgentCard>` displays: agent name, personality label, model name, uptime duration, health indicator (green/yellow/red traffic light), current status text, and iteration count
- [ ] Agent data is fetched from the Hive server's `/agent list` equivalent endpoint on tab activation and refreshed via WebSocket events
- [ ] Health indicators reflect agent state: green = running and responsive, yellow = high context usage or restarting, red = crashed or unreachable
- [ ] Cards are laid out in a responsive grid: 3 columns on large screens, 2 on medium, 1 on small
- [ ] A summary bar above the grid shows total agent count and breakdown by health status (e.g., "12 agents: 9 healthy, 2 warning, 1 error")
- [ ] Clicking an agent card selects it and opens the context panel with expanded details: recent messages sent by the agent, room assignments, and full status history
- [ ] Empty state: when no agents are running, display a message indicating no active agents (spawn wizard link deferred to FE-008)

## Phase
Phase 1: Web Dashboard MVP

## Priority
P0

## Components
- AgentCard
- AgentGrid

## Notes
This is a read-only view in Phase 1. Agent management actions (spawn, stop, restart) are added in Phase 2 (FE-008, FE-009). The traffic light health indicator pattern is inspired by Kubernetes Dashboard pod status. Agent data source depends on the Hive server agent registry API.
