# [FE-012] Draggable Task Cards for Assignment

**As a** workspace admin or host
**I want to** drag task cards between kanban columns to change their status or assign them
**So that** I can manage task workflow without typing slash commands

## Acceptance Criteria
- [ ] Task cards in the `<TaskBoard>` are draggable using pointer (mouse/touch) input with a visual drag preview showing the card in a semi-transparent lifted state
- [ ] Dropping a card onto the "Claimed" column prompts for an assignee (dropdown of active workspace members) and sends a `/taskboard assign <task-id> <username>` command to the server
- [ ] Dropping a card onto the "Done" column sends a `/taskboard finish <task-id>` command; only the current assignee or host can perform this action
- [ ] Invalid drops (e.g., skipping required statuses, unauthorized user) snap the card back to its original column with a toast notification explaining why the action was rejected
- [ ] Drag handles are visible on hover to indicate draggability; cards also support a right-click context menu with the same status transition actions as alternatives to drag-and-drop
- [ ] The board applies optimistic updates during the drop (card moves immediately) and reverts if the server rejects the command
- [ ] Keyboard accessibility: cards can be selected with Enter/Space and moved between columns with arrow keys, confirming with Enter
- [ ] Drag-and-drop is disabled for users without admin/host permissions; they see the board as read-only

## Phase
Phase 2: Interactive Features

## Priority
P1

## Components
- TaskBoard
- TaskCard

## Notes
Drag-and-drop should use a library compatible with the chosen framework (e.g., @dnd-kit for React, svelte-dnd-action for Svelte). The valid status transitions follow the taskboard lifecycle: Open -> Claimed -> Planned -> Approved -> Finished, with release back to Open. Not all transitions are valid via drag (e.g., "plan" requires text input, so dragging to Planned should open an inline form).
