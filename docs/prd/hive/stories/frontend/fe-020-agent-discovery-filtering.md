# [FE-020] Agent Discovery and Filtering by Domain/Capacity

**As a** workspace admin
**I want to** search and filter agents by their domain expertise and available capacity
**So that** I can find the right agent for a task and identify underutilized agents

## Acceptance Criteria
- [ ] The Agents view includes a filter bar above the `<AgentGrid>` with the following filter controls: domain/personality (dropdown with multi-select), model type (dropdown), health status (checkbox group: healthy/warning/error/stopped), and room assignment (dropdown)
- [ ] A text search field filters agents by name, personality label, or current status text (client-side, real-time as the user types)
- [ ] Filters are combinable (AND logic): selecting "personality=debugger" and "status=healthy" shows only healthy debugger agents
- [ ] Each agent card displays a capacity indicator: current context usage as a percentage bar (from room-ralph's context monitoring data), with color coding (green <50%, yellow 50-80%, red >80%)
- [ ] Agents can be sorted by: name (A-Z), uptime (longest first), context usage (most available first), or cost (highest first) via a sort dropdown
- [ ] Filter and sort state is reflected in the URL query string so that filtered views are shareable/bookmarkable
- [ ] A "Available for assignment" quick filter shows only agents that are healthy, have <50% context usage, and are not currently assigned to a task
- [ ] Empty state after filtering: display "No agents match filters" with a "Clear filters" button

## Phase
Phase 4: Advanced

## Priority
P2

## Components
- AgentGrid
- AgentCard

## Notes
Agent discovery depends on the Hive server's agent registry and the prd-agent-discovery.md specification. Domain information comes from personality definitions. Capacity data (context usage) comes from room-ralph's monitoring output. This builds on the read-only agent list from FE-006 by adding interactive filtering and search.
