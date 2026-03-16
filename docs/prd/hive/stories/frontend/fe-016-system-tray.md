# [FE-016] System Tray Icon with Agent Health Summary

**As a** desktop user
**I want to** see agent health status from the system tray without opening the main window
**So that** I can monitor my agent fleet at a glance while working in other applications

## Acceptance Criteria
- [ ] The Tauri app places an icon in the system tray (macOS menu bar, Windows notification area, Linux system tray) that persists when the main window is closed
- [ ] The tray icon color reflects aggregate agent health: green = all agents healthy, yellow = one or more agents in warning state, red = one or more agents in error/crashed state, gray = no agents running or disconnected from server
- [ ] Clicking the tray icon opens a compact popup menu showing: each agent's name and health status (colored dot), a separator, and menu items for "Open Hive", "Spawn Agent...", and "Quit"
- [ ] The popup menu updates in real-time as agent health changes via the WebSocket connection (the connection is maintained even when the main window is hidden)
- [ ] Right-clicking the tray icon (or Ctrl+click on macOS) opens a context menu with: "Show/Hide Window", "Preferences", and "Quit"
- [ ] When all agents are healthy, the tray tooltip shows "Hive - N agents running"; when issues exist, it shows the count of unhealthy agents
- [ ] Closing the main window minimizes to tray instead of quitting the application (configurable in preferences; default: minimize to tray)

## Phase
Phase 3: Tauri Desktop

## Priority
P1

## Components
- AppShell (Tauri integration)

## Notes
Tauri v2 provides system tray APIs via the `tray-icon` plugin. The tray icon must work across all three platforms (macOS, Windows, Linux). The WebSocket connection must remain active when the window is hidden to keep the health data current. Icon assets are needed for each state (green, yellow, red, gray) in platform-appropriate formats.
