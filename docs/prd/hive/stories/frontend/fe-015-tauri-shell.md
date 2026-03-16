# [FE-015] Tauri Shell Wrapping the Web App

**As a** desktop user
**I want to** run Hive as a native desktop application
**So that** I get a dedicated window, OS-level integration, and faster startup without opening a browser

## Acceptance Criteria
- [ ] The existing web frontend builds and runs inside a Tauri v2 webview with zero code changes to the web application layer
- [ ] The Tauri app window opens at a configurable default size (1280x800) with proper minimum size constraints (800x600) and remembers its last position/size across launches
- [ ] The app registers a custom protocol handler (`hive://`) so that clicking room links in other applications opens the desktop app to the correct room
- [ ] The Tauri build produces distributable binaries for macOS (.dmg), Windows (.msi), and Linux (.AppImage/.deb) via the Tauri bundler
- [ ] The app displays a native title bar with the Hive icon and room/workspace name; on macOS, traffic light buttons are properly positioned
- [ ] Dev mode (`tauri dev`) provides hot module replacement with the same Vite HMR experience as the web version
- [ ] The app can be opened from the command line: `hive-desktop` launches the app, `hive-desktop --url <server>` pre-fills the server URL on the login page
- [ ] Application auto-update checks for new versions on startup and prompts the user to install (using Tauri's updater plugin)

## Phase
Phase 3: Tauri Desktop

## Priority
P1

## Components
- AppShell

## Notes
Per the PRD, Tauri v2 wraps the same web frontend with zero rewrite. The Tauri configuration (tauri.conf.json) needs to allow WebSocket connections to arbitrary Hive server URLs (CSP adjustments). File system access for workspace management is a Phase 3 goal but not part of this story -- it will be a separate story if needed.
