# [FE-022] Dark Mode / Theming Support

**As a** user
**I want to** switch between light and dark themes (or follow my OS preference)
**So that** I can use Hive comfortably in any lighting condition and match my system appearance

## Acceptance Criteria
- [ ] A theme toggle is accessible from the app shell header (sun/moon icon) with three options: Light, Dark, and System (follows OS preference)
- [ ] Dark mode applies a cohesive dark color palette across all components: backgrounds, text, borders, cards, charts, badges, and interactive elements
- [ ] The theme preference is stored in localStorage and applied immediately on page load (no flash of wrong theme)
- [ ] All color values are defined as CSS custom properties (or Tailwind theme tokens) so that a single theme swap changes the entire UI consistently
- [ ] Color contrast ratios meet WCAG AA standards in both themes: minimum 4.5:1 for normal text and 3:1 for large text and UI components
- [ ] Syntax highlighting in code blocks (chat messages) and the log viewer adapts to the active theme
- [ ] Charts in the Costs view (`<CostChart>`) use theme-aware color palettes that are legible in both light and dark modes
- [ ] The Tauri desktop version (FE-015) respects the same theme setting and applies it to the native title bar where possible (macOS vibrancy, Windows Mica)

## Phase
Phase 4: Advanced

## Priority
P2

## Components
- AppShell
- ChatTimeline
- CostChart
- LogViewer

## Notes
Tailwind CSS supports dark mode via the `dark:` variant (class strategy recommended over media strategy for manual toggle support). The "System" option uses `prefers-color-scheme` media query and a MutationObserver or matchMedia listener to react to OS-level changes. Custom theming beyond light/dark (e.g., user-defined accent colors) is out of scope for this story.
