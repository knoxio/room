# [FE-021] Team Creation Wizard from Manifest

**As a** workspace admin
**I want to** create an entire agent team from a manifest file through a guided wizard
**So that** I can spin up pre-configured teams for sprints without manually spawning each agent

## Acceptance Criteria
- [ ] A "Create Team" button in the Agents view opens a `<TeamWizard>` modal dialog
- [ ] Step 1 — Manifest input: the user can paste a YAML/JSON manifest or upload a manifest file; the wizard parses it and displays a preview of the team composition (agent names, personalities, models, room assignments)
- [ ] Step 2 — Review and customize: each agent in the manifest is shown as an editable row where the user can override the personality, model, or target room before creation
- [ ] Step 3 — Room setup: the wizard auto-creates rooms listed in the manifest that do not already exist, with a checkbox to skip room creation for existing rooms
- [ ] Step 4 — Confirm and deploy: a summary shows the full team configuration; on confirmation, the wizard sends parallel spawn requests and displays a progress tracker showing each agent's startup status (pending/starting/healthy/failed)
- [ ] Manifest validation: invalid manifests (missing required fields, unknown personalities, invalid room names) show specific inline error messages at the field level
- [ ] If any agent fails to spawn, the wizard shows which agents succeeded and which failed, with error details and a "Retry failed" button
- [ ] Previously used manifests are saved to localStorage and available in a "Recent manifests" dropdown for quick re-use
- [ ] The manifest format is documented with an inline help link or expandable example within the wizard

## Phase
Phase 4: Advanced

## Priority
P2

## Components
- SpawnWizard (extended)
- AgentGrid

## Notes
The manifest format should align with the team provisioning PRD (prd-team-provisioning.md). This wizard orchestrates multiple spawn operations from FE-008. The progress tracker should update in real-time as agents come online via WebSocket health events. Consider supporting a "dry run" mode that validates the manifest without actually spawning agents.
