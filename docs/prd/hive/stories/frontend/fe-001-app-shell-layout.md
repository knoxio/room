# [FE-001] App Shell with Three-Panel Layout and Tab Navigation

**As a** Hive user
**I want to** see a three-panel layout with top-level tab navigation
**So that** I can navigate between Rooms, Agents, Tasks, and Costs views without losing context

## Acceptance Criteria
- [ ] The `<AppShell>` component renders three panels: left sidebar (fixed-width, collapsible), main content (fluid), and context panel (fixed-width, collapsible)
- [ ] Top-level tab bar renders four tabs: Rooms, Agents, Tasks, Costs
- [ ] Clicking a tab switches the main content area to the corresponding view without a full page reload
- [ ] The active tab is visually highlighted and the URL path updates to reflect the current view (e.g., `/rooms`, `/agents`, `/tasks`, `/costs`)
- [ ] The layout is responsive: on viewports below 768px, the sidebar collapses to an icon rail and the context panel is hidden (accessible via overlay)
- [ ] Panel widths are adjustable via drag handles between panels, and the chosen widths persist across page reloads (localStorage)
- [ ] Keyboard shortcut Ctrl+1/2/3/4 switches between tabs

## Phase
Phase 1: Web Dashboard MVP

## Priority
P0

## Components
- AppShell

## Notes
The three-panel layout follows the pattern described in the PRD (left sidebar, main content, context panel). Tailwind CSS handles responsive breakpoints. The sidebar and context panel collapse states should be stored in Zustand/Svelte store and persisted to localStorage.
