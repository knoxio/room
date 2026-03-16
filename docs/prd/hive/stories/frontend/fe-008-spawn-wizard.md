# [FE-008] Agent Spawn Wizard

**As a** workspace admin
**I want to** spawn a new agent through a guided wizard UI
**So that** I can deploy agents without using the terminal CLI

## Acceptance Criteria
- [ ] The `<SpawnWizard>` component is accessible from the Agents view via a "Spawn Agent" button and opens as a modal dialog
- [ ] Step 1 — Identity: user enters agent username, selects a personality from a dropdown (populated from available personality definitions), and optionally provides a custom prompt override
- [ ] Step 2 — Configuration: user selects the model (e.g., claude-sonnet, claude-opus), target room(s) to join, and optional tool restrictions (allowed/disallowed tools)
- [ ] Step 3 — Review: a summary of all selections is displayed before confirmation, with the ability to go back and edit any step
- [ ] On confirmation, the wizard sends a spawn request to the Hive server API and displays a progress indicator until the agent reports as healthy
- [ ] Validation errors are shown inline: duplicate username, invalid room selection, missing required fields
- [ ] The wizard can be cancelled at any step without side effects
- [ ] After successful spawn, the new agent appears in the `<AgentGrid>` with a "Starting..." status that transitions to healthy once the agent's first heartbeat arrives

## Phase
Phase 2: Interactive Features

## Priority
P1

## Components
- SpawnWizard

## Notes
Personality definitions come from the personality system (see prd-personality.md). Model options come from the Hive server's available models endpoint. The spawn request maps to the `/agent spawn` command or equivalent Hive server API. The wizard should prevent spawning agents with names that collide with existing usernames in the workspace.
