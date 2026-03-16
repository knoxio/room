# [FE-014] Message Input with Command Palette for Slash Commands

**As a** user
**I want to** send messages and execute slash commands from a rich input field
**So that** I can interact with rooms and agents without switching to the terminal

## Acceptance Criteria
- [ ] A message input bar is rendered at the bottom of the chat timeline in the Rooms view, with a text area that supports multi-line input (Shift+Enter for newline, Enter to send)
- [ ] Typing `/` at the start of the input triggers a command palette popup listing available slash commands (sourced from `builtin_command_infos()` and plugin commands)
- [ ] The command palette filters results as the user types (e.g., `/task` shows `/taskboard post`, `/taskboard list`, etc.) and supports keyboard navigation (arrow keys + Enter to select)
- [ ] Selecting a command from the palette auto-fills it into the input and shows parameter hints (name, type, required/optional) inline below the input field
- [ ] Messages are sent via the WebSocket connection and echo back through the normal message flow (no local injection to prevent desyncs)
- [ ] The input supports @mention autocomplete: typing `@` shows a filterable list of room members; selecting one inserts the username and highlights it
- [ ] Input history: up/down arrow keys cycle through previously sent messages (last 50, stored in sessionStorage)
- [ ] The send button is disabled and the input shows a subtle loading state while a command is being processed

## Phase
Phase 2: Interactive Features

## Priority
P1

## Components
- ChatTimeline (input area)

## Notes
The command palette mirrors the TUI's `CommandPalette` widget behavior but adapted for a graphical interface. Command metadata (parameter schemas) comes from the `ParamSchema`/`ParamType` types in the plugin system. The palette should be keyboard-first but also work with mouse/touch clicks.
