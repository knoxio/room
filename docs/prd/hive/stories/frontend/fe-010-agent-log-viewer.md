# [FE-010] Agent Log Viewer (Streaming)

**As a** workspace admin
**I want to** view an agent's logs in real-time within the dashboard
**So that** I can diagnose issues without SSH-ing into the server or tailing log files

## Acceptance Criteria
- [ ] The `<LogViewer>` component opens in the context panel (right panel) when an agent card is clicked and the "Logs" tab is selected
- [ ] Logs stream in real-time via a dedicated WebSocket channel or multiplexed over the existing connection, appending new lines as they arrive
- [ ] Each log line displays: timestamp, log level (color-coded: gray=debug, white=info, yellow=warn, red=error), and the log message
- [ ] The viewer auto-scrolls to the latest log line when the user is at the bottom; manual scroll-up pauses auto-scroll and shows a "Jump to bottom" button
- [ ] A log level filter dropdown allows toggling visibility of debug/info/warn/error levels independently
- [ ] A text search box filters log lines client-side, highlighting matching terms in the visible output
- [ ] The viewer supports loading historical logs (last N lines) on initial open, with upward scroll to load more
- [ ] A "Copy All" button copies the currently visible (filtered) log lines to the clipboard

## Phase
Phase 2: Interactive Features

## Priority
P1

## Components
- LogViewer

## Notes
Log data source depends on the Hive server's agent log streaming API. Logs may come from room-ralph's stdout/stderr or a structured logging endpoint. The viewer should handle high-throughput log streams (100+ lines/second) without dropping frames -- consider virtualized rendering for the log list. ANSI color codes in raw logs should be stripped or converted to CSS classes.
