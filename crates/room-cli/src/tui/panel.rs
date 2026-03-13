//! Tab bar and member panel rendering for the TUI.
//!
//! Contains the tab bar (multi-room tab strip) and floating member panel
//! (online users with status and subscription tier indicators).

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use room_protocol::SubscriptionTier;

use super::colors::{user_color, ColorMap};

/// Describes a single tab for the tab bar renderer.
pub(crate) struct TabInfo {
    pub(crate) room_id: String,
    pub(crate) active: bool,
    pub(crate) unread: usize,
}

/// Render the tab bar as a single `Line` of styled spans.
///
/// Hidden when there is only one tab (backward-compatible single-room mode).
/// Active tab is highlighted; inactive tabs with unread messages show a count badge.
pub(crate) fn render_tab_bar(tabs: &[TabInfo]) -> Option<Line<'static>> {
    if tabs.len() <= 1 {
        return None;
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));
    for tab in tabs {
        let label = if tab.unread > 0 && !tab.active {
            format!(" {} ({}) ", tab.room_id, tab.unread)
        } else {
            format!(" {} ", tab.room_id)
        };
        let style = if tab.active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if tab.unread > 0 {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    Some(Line::from(spans))
}

/// Build the styled spans for a single member panel row.
///
/// Renders: ` <username>` (bold, colored) + optional tier indicator + optional
/// status (dimmed) + trailing space. Used by the floating member panel.
pub(crate) fn build_member_panel_spans(
    username: &str,
    status: &str,
    tier: Option<SubscriptionTier>,
    color_map: &ColorMap,
) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!(" {username}"),
        Style::default()
            .fg(user_color(username, color_map))
            .add_modifier(Modifier::BOLD),
    )];
    match tier {
        Some(SubscriptionTier::MentionsOnly) => {
            spans.push(Span::styled(" @", Style::default().fg(Color::Yellow)));
        }
        Some(SubscriptionTier::Unsubscribed) => {
            spans.push(Span::styled(" \u{2717}", Style::default().fg(Color::Red)));
        }
        _ => {}
    }
    if !status.is_empty() {
        spans.push(Span::styled(
            format!("  {status}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::raw(" "));
    spans
}

/// Compute the content width of a single member panel row.
///
/// Returns the number of characters needed to display the username, tier
/// indicator, and status for one row. Used to size the floating panel.
pub(crate) fn member_panel_row_width(
    username: &str,
    status: &str,
    tier: Option<SubscriptionTier>,
) -> usize {
    let tier_len = match tier {
        Some(SubscriptionTier::MentionsOnly) | Some(SubscriptionTier::Unsubscribed) => 2,
        _ => 0,
    };
    let status_len = if status.is_empty() {
        0
    } else {
        status.len() + 2 // "  " + status
    };
    username.len() + 1 + tier_len + status_len + 1 // " " + name + tier + status + " "
}

/// Truncate a status string to fit within `max_chars` characters.
///
/// If the status is longer than `max_chars`, it is cut and an ellipsis (`…`)
/// is appended. The returned string is at most `max_chars` characters wide.
/// Returns the original string unchanged if it already fits.
pub(crate) fn ellipsize_status(status: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if status.chars().count() <= max_chars {
        return status.to_owned();
    }
    // Leave room for the ellipsis character.
    let truncated: String = status.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}\u{2026}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── render_tab_bar tests ──────────────────────────────────────────────

    #[test]
    fn tab_bar_hidden_for_single_tab() {
        let tabs = vec![TabInfo {
            room_id: "room-1".into(),
            active: true,
            unread: 0,
        }];
        assert!(render_tab_bar(&tabs).is_none());
    }

    #[test]
    fn tab_bar_hidden_for_empty_tabs() {
        let tabs: Vec<TabInfo> = vec![];
        assert!(render_tab_bar(&tabs).is_none());
    }

    #[test]
    fn tab_bar_shown_for_multiple_tabs() {
        let tabs = vec![
            TabInfo {
                room_id: "alpha".into(),
                active: true,
                unread: 0,
            },
            TabInfo {
                room_id: "beta".into(),
                active: false,
                unread: 0,
            },
        ];
        let line = render_tab_bar(&tabs).expect("should render tab bar");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("alpha"), "tab bar should contain 'alpha'");
        assert!(text.contains("beta"), "tab bar should contain 'beta'");
    }

    #[test]
    fn tab_bar_shows_unread_badge_on_inactive_tab() {
        let tabs = vec![
            TabInfo {
                room_id: "alpha".into(),
                active: true,
                unread: 0,
            },
            TabInfo {
                room_id: "beta".into(),
                active: false,
                unread: 5,
            },
        ];
        let line = render_tab_bar(&tabs).expect("should render tab bar");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("beta (5)"),
            "inactive tab with unread should show count badge"
        );
    }

    #[test]
    fn tab_bar_no_unread_badge_on_active_tab() {
        let tabs = vec![
            TabInfo {
                room_id: "alpha".into(),
                active: true,
                unread: 3,
            },
            TabInfo {
                room_id: "beta".into(),
                active: false,
                unread: 0,
            },
        ];
        let line = render_tab_bar(&tabs).expect("should render tab bar");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        // Active tab should NOT show unread badge even if count > 0
        assert!(
            !text.contains("alpha (3)"),
            "active tab should not show unread badge"
        );
    }

    #[test]
    fn tab_bar_active_tab_has_bold_cyan_style() {
        let tabs = vec![
            TabInfo {
                room_id: "alpha".into(),
                active: true,
                unread: 0,
            },
            TabInfo {
                room_id: "beta".into(),
                active: false,
                unread: 0,
            },
        ];
        let line = render_tab_bar(&tabs).unwrap();
        // Find the span containing "alpha"
        let alpha_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("alpha"))
            .expect("should find alpha span");
        assert_eq!(alpha_span.style.fg, Some(Color::Black));
        assert_eq!(alpha_span.style.bg, Some(Color::Cyan));
    }

    // ── build_member_panel_spans tests ─────────────────────────────────────

    #[test]
    fn member_panel_spans_plain_user() {
        let cm = ColorMap::new();
        let spans = build_member_panel_spans("alice", "", None, &cm);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " alice ");
    }

    #[test]
    fn member_panel_spans_with_status() {
        let cm = ColorMap::new();
        let spans = build_member_panel_spans("alice", "coding", None, &cm);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " alice  coding ");
    }

    #[test]
    fn member_panel_spans_mentions_only_tier() {
        let cm = ColorMap::new();
        let spans = build_member_panel_spans("bob", "", Some(SubscriptionTier::MentionsOnly), &cm);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " bob @ ");
        // Verify the "@" indicator has yellow color
        let at_span = spans.iter().find(|s| s.content.contains('@')).unwrap();
        assert_eq!(at_span.style.fg, Some(Color::Yellow));
    }

    #[test]
    fn member_panel_spans_unsubscribed_tier() {
        let cm = ColorMap::new();
        let spans =
            build_member_panel_spans("charlie", "", Some(SubscriptionTier::Unsubscribed), &cm);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('\u{2717}'), "should contain cross mark");
        // Verify the cross mark has red color
        let cross_span = spans
            .iter()
            .find(|s| s.content.contains('\u{2717}'))
            .unwrap();
        assert_eq!(cross_span.style.fg, Some(Color::Red));
    }

    #[test]
    fn member_panel_spans_full_tier_no_indicator() {
        let cm = ColorMap::new();
        let spans = build_member_panel_spans("dave", "", Some(SubscriptionTier::Full), &cm);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " dave ");
    }

    #[test]
    fn member_panel_spans_with_status_and_tier() {
        let cm = ColorMap::new();
        let spans = build_member_panel_spans(
            "eve",
            "reviewing PR",
            Some(SubscriptionTier::MentionsOnly),
            &cm,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " eve @  reviewing PR ");
    }

    #[test]
    fn member_panel_spans_username_is_bold() {
        let cm = ColorMap::new();
        let spans = build_member_panel_spans("alice", "", None, &cm);
        let name_span = &spans[0];
        assert!(
            name_span.style.add_modifier.contains(Modifier::BOLD),
            "username should be bold"
        );
    }

    #[test]
    fn member_panel_spans_status_is_dimmed() {
        let cm = ColorMap::new();
        let spans = build_member_panel_spans("alice", "busy", None, &cm);
        let status_span = spans.iter().find(|s| s.content.contains("busy")).unwrap();
        assert_eq!(
            status_span.style.fg,
            Some(Color::DarkGray),
            "status should be DarkGray"
        );
    }

    // ── member_panel_row_width tests ────────────────────────────────────────

    #[test]
    fn row_width_plain_user() {
        // " alice " = 1 + 5 + 1 = 7
        assert_eq!(member_panel_row_width("alice", "", None), 7);
    }

    #[test]
    fn row_width_with_status() {
        // " alice  coding " = 1 + 5 + 0 + (2 + 6) + 1 = 15
        assert_eq!(member_panel_row_width("alice", "coding", None), 15);
    }

    #[test]
    fn row_width_with_mentions_only_tier() {
        // " bob @ " = 1 + 3 + 2 + 0 + 1 = 7
        assert_eq!(
            member_panel_row_width("bob", "", Some(SubscriptionTier::MentionsOnly)),
            7
        );
    }

    #[test]
    fn row_width_with_unsubscribed_tier() {
        // Same as MentionsOnly: +2 for the indicator
        assert_eq!(
            member_panel_row_width("bob", "", Some(SubscriptionTier::Unsubscribed)),
            7
        );
    }

    #[test]
    fn row_width_full_tier_no_extra() {
        assert_eq!(
            member_panel_row_width("bob", "", Some(SubscriptionTier::Full)),
            member_panel_row_width("bob", "", None),
        );
    }

    #[test]
    fn row_width_with_status_and_tier() {
        // " eve @  reviewing PR " = 1 + 3 + 2 + (2 + 12) + 1 = 21
        assert_eq!(
            member_panel_row_width("eve", "reviewing PR", Some(SubscriptionTier::MentionsOnly)),
            21
        );
    }

    // ── ellipsize_status tests ──────────────────────────────────────────────

    #[test]
    fn ellipsize_fits_unchanged() {
        assert_eq!(ellipsize_status("coding", 10), "coding");
    }

    #[test]
    fn ellipsize_exact_length_unchanged() {
        assert_eq!(ellipsize_status("coding", 6), "coding");
    }

    #[test]
    fn ellipsize_truncates_with_ellipsis() {
        let result = ellipsize_status("implementing feature X", 10);
        assert_eq!(result, "implement\u{2026}");
        assert_eq!(result.chars().count(), 10);
    }

    #[test]
    fn ellipsize_max_one_returns_ellipsis() {
        assert_eq!(ellipsize_status("hello", 1), "\u{2026}");
    }

    #[test]
    fn ellipsize_max_zero_returns_empty() {
        assert_eq!(ellipsize_status("hello", 0), "");
    }

    #[test]
    fn ellipsize_empty_status_unchanged() {
        assert_eq!(ellipsize_status("", 10), "");
    }

    #[test]
    fn ellipsize_unicode_status() {
        // 5 chars: "日本語テスト" = 6 chars, truncate to 5 → "日本語テ…"
        let result = ellipsize_status("日本語テスト", 5);
        assert_eq!(result.chars().count(), 5);
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn ellipsize_max_two() {
        let result = ellipsize_status("hello", 2);
        assert_eq!(result, "h\u{2026}");
        assert_eq!(result.chars().count(), 2);
    }
}
