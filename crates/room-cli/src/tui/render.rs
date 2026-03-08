use std::collections::{HashMap, HashSet};

use chrono::Local;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

use crate::message::Message;

/// Color palette for user names.
const PALETTE: &[Color] = &[
    Color::Yellow,
    Color::Cyan,
    Color::Green,
    Color::Magenta,
    Color::LightYellow,
    Color::LightCyan,
    Color::LightGreen,
    Color::LightMagenta,
    Color::LightRed,
    Color::LightBlue,
];

/// Persistent map of username -> assigned color. Stored in TUI state.
pub(super) type ColorMap = HashMap<String, Color>;

/// Assign a color to a username, preferring unused palette colors.
///
/// If the user already has a color, returns it. Otherwise picks the
/// hash-preferred color if available, or the first unused palette color.
/// Falls back to the hash color when all palette slots are taken.
pub(super) fn assign_color(username: &str, color_map: &mut ColorMap) -> Color {
    if let Some(&color) = color_map.get(username) {
        return color;
    }
    let used: HashSet<Color> = color_map.values().copied().collect();
    let hash = username.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as usize)
    });
    let preferred = PALETTE[hash % PALETTE.len()];
    if !used.contains(&preferred) {
        color_map.insert(username.to_owned(), preferred);
        return preferred;
    }
    // Hash color is taken — find first unused palette color.
    for &color in PALETTE {
        if !used.contains(&color) {
            color_map.insert(username.to_owned(), color);
            return color;
        }
    }
    // All palette colors used — accept collision with hash color.
    color_map.insert(username.to_owned(), preferred);
    preferred
}

/// Arrow glyph used in DM display (`→`).
const DM_ARROW: &str = "\u{2192}";

/// Build the welcome splash as centered, styled `Text`.
///
/// `frame` drives two animations:
/// - An antenna light on top blinks between ✦ (bright) and · (dim).
/// - The right eye winks every other cycle (◉ → ‿).
/// - The "room" label pulses between bright cyan and dim gray.
///
/// Below the logo: tagline, version, and today's date.
pub(super) fn welcome_splash(frame: usize, width: usize) -> Text<'static> {
    const BLINK_INTERVAL: usize = 10; // ~500ms at 50ms/frame
    let phase = frame / BLINK_INTERVAL;
    let antenna_on = phase.is_multiple_of(2);
    let wink = !antenna_on;

    // Antenna line: light + stem
    let antenna_light = if antenna_on { "✦" } else { "·" };
    let antenna_line = format!("     {antenna_light}     ");
    let stem_line = "     │     ";

    // Face lines
    let eye_line = if wink {
        "│ (◉)(‿)  │"
    } else {
        "│ (◉)(◉)  │"
    };

    let light_style = if antenna_on {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let label_style = if antenna_on {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let box_style = Style::default().fg(Color::Cyan);

    let raw_lines: Vec<(&str, Style)> = vec![
        (&antenna_line, light_style),
        (stem_line, box_style),
        ("╭─────────╮", box_style),
        (eye_line, box_style),
        ("│  ╰──╯   │", box_style),
        ("╰────┬────╯", box_style),
        ("  r o o m  ", label_style),
    ];

    let mut lines: Vec<Line<'static>> = Vec::new();

    let v_pad = 1;
    for _ in 0..v_pad {
        lines.push(Line::from(""));
    }

    for (raw, style) in &raw_lines {
        let display_width = raw.chars().count();
        let pad = width.saturating_sub(display_width) / 2;
        let padding = " ".repeat(pad);
        lines.push(Line::from(vec![
            Span::raw(padding),
            Span::styled(raw.to_string(), *style),
        ]));
    }

    // Blank separator
    lines.push(Line::from(""));

    // Tagline
    let tagline = "Agent coordination for humans";
    let tagline_pad = " ".repeat(width.saturating_sub(tagline.len()) / 2);
    lines.push(Line::from(vec![
        Span::raw(tagline_pad),
        Span::styled(tagline.to_string(), Style::default().fg(Color::DarkGray)),
    ]));

    // Version + date
    let version = env!("CARGO_PKG_VERSION");
    let today = Local::now().format("%Y-%m-%d");
    let info = format!("v{version}  ·  {today}");
    let info_pad = " ".repeat(width.saturating_sub(info.len()) / 2);
    lines.push(Line::from(vec![
        Span::raw(info_pad),
        Span::styled(info, Style::default().fg(Color::DarkGray)),
    ]));

    Text::from(lines)
}

/// Split a message content string into styled spans, rendering `@username`
/// tokens in the mentioned user's colour.
pub(super) fn render_content_with_mentions(
    content: &str,
    color_map: &ColorMap,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut remaining = content;
    while let Some(at_pos) = remaining.find('@') {
        if at_pos > 0 {
            spans.push(Span::raw(remaining[..at_pos].to_string()));
        }
        let after_at = &remaining[at_pos + 1..];
        let username_end = after_at
            .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .unwrap_or(after_at.len());
        if username_end == 0 {
            // Bare '@' with no username — treat as literal text.
            spans.push(Span::raw("@".to_string()));
            remaining = after_at;
        } else {
            let username = &after_at[..username_end];
            spans.push(Span::styled(
                format!("@{username}"),
                Style::default()
                    .fg(user_color(username, color_map))
                    .add_modifier(Modifier::BOLD),
            ));
            remaining = &after_at[username_end..];
        }
    }
    if !remaining.is_empty() {
        spans.push(Span::raw(remaining.to_string()));
    }
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

/// Word-wrap `text` so that no line exceeds `width` characters.
///
/// Explicit `\n` characters in `text` are preserved as hard line breaks.
/// Words longer than `width` are hard-split at the column boundary.
/// If `width` is 0 the text is returned as a single unsplit chunk.
pub(super) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut all_chunks: Vec<String> = Vec::new();
    for logical_line in text.split('\n') {
        let mut current = String::new();
        for word in logical_line.split_whitespace() {
            if current.is_empty() {
                // Hard-split any word that is longer than the available width.
                let mut w = word;
                while w.chars().count() > width {
                    let split_idx = w
                        .char_indices()
                        .nth(width)
                        .map(|(i, _)| i)
                        .unwrap_or(w.len());
                    all_chunks.push(w[..split_idx].to_string());
                    w = &w[split_idx..];
                }
                current = w.to_string();
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                all_chunks.push(std::mem::take(&mut current));
                let mut w = word;
                while w.chars().count() > width {
                    let split_idx = w
                        .char_indices()
                        .nth(width)
                        .map(|(i, _)| i)
                        .unwrap_or(w.len());
                    all_chunks.push(w[..split_idx].to_string());
                    w = &w[split_idx..];
                }
                current = w.to_string();
            }
        }
        // Push remaining content for this logical line (may be empty for blank lines).
        all_chunks.push(current);
    }
    all_chunks
}

pub(super) fn format_message(
    msg: &Message,
    available_width: usize,
    color_map: &ColorMap,
) -> Text<'static> {
    match msg {
        Message::Join { ts, user, .. } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Text::from(Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user} joined"),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]))
        }
        Message::Leave { ts, user, .. } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Text::from(Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user} left"),
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]))
        }
        Message::Message {
            ts, user, content, ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            let prefix_plain = format!("[{ts_str}] {user}: ");
            let prefix_width = prefix_plain.chars().count();
            let content_width = available_width.saturating_sub(prefix_width);
            let chunks = wrap_words(content, content_width);
            let indent = " ".repeat(prefix_width);
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i == 0 {
                    let mut line_spans = vec![
                        Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("{user}: "),
                            Style::default()
                                .fg(user_color(user, color_map))
                                .add_modifier(Modifier::BOLD),
                        ),
                    ];
                    line_spans.extend(render_content_with_mentions(&chunk, color_map));
                    lines.push(Line::from(line_spans));
                } else {
                    let mut line_spans = vec![Span::raw(indent.clone())];
                    line_spans.extend(render_content_with_mentions(&chunk, color_map));
                    lines.push(Line::from(line_spans));
                }
            }
            Text::from(lines)
        }
        Message::Reply {
            ts,
            user,
            reply_to,
            content,
            ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            let short_id = &reply_to[..reply_to.len().min(8)];
            let prefix_plain = format!("[{ts_str}] {user}: (re:{short_id}) ");
            let prefix_width = prefix_plain.chars().count();
            let content_width = available_width.saturating_sub(prefix_width);
            let chunks = wrap_words(content, content_width);
            let indent = " ".repeat(prefix_width);
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i == 0 {
                    let mut line_spans = vec![
                        Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("{user}: "),
                            Style::default()
                                .fg(user_color(user, color_map))
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("(re:{short_id}) "),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ];
                    line_spans.extend(render_content_with_mentions(&chunk, color_map));
                    lines.push(Line::from(line_spans));
                } else {
                    let mut line_spans = vec![Span::raw(indent.clone())];
                    line_spans.extend(render_content_with_mentions(&chunk, color_map));
                    lines.push(Line::from(line_spans));
                }
            }
            Text::from(lines)
        }
        Message::Command {
            ts,
            user,
            cmd,
            params,
            ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Text::from(Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user}: "),
                    Style::default()
                        .fg(user_color(user, color_map))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("/{cmd} {}", params.join(" ")),
                    Style::default().fg(Color::Magenta),
                ),
            ]))
        }
        Message::System { ts, content, .. } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            let prefix_plain = format!("[{ts_str}] [system] ");
            let prefix_width = prefix_plain.chars().count();
            let content_width = available_width.saturating_sub(prefix_width);
            let chunks = wrap_words(content, content_width);
            let indent = " ".repeat(prefix_width);
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("[system] {chunk}"),
                            Style::default().fg(Color::Cyan),
                        ),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw(indent.clone()),
                        Span::styled(chunk, Style::default().fg(Color::Cyan)),
                    ]));
                }
            }
            Text::from(lines)
        }
        Message::DirectMessage {
            ts,
            user,
            to,
            content,
            ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            let prefix_plain = format!("[{ts_str}] [dm] {user}{DM_ARROW}{to}: ");
            let prefix_width = prefix_plain.chars().count();
            let content_width = available_width.saturating_sub(prefix_width);
            let chunks = wrap_words(content, content_width);
            let indent = " ".repeat(prefix_width);
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            "[dm] ",
                            Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{user}{DM_ARROW}{to}: "),
                            Style::default()
                                .fg(user_color(user, color_map))
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(chunk),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw(indent.clone()),
                        Span::raw(chunk),
                    ]));
                }
            }
            Text::from(lines)
        }
    }
}

/// Given per-message visual line heights and a target top line index,
/// returns `(message_index, lines_to_skip)` — the first message that
/// should appear in the viewport and how many of its leading lines to drop.
pub(super) fn find_view_start(heights: &[usize], view_top: usize) -> (usize, usize) {
    let mut accum = 0usize;
    for (i, &h) in heights.iter().enumerate() {
        if accum + h > view_top {
            return (i, view_top - accum);
        }
        accum += h;
    }
    (heights.len(), 0)
}

/// Describes a single tab for the tab bar renderer.
pub(super) struct TabInfo {
    pub(super) room_id: String,
    pub(super) active: bool,
    pub(super) unread: usize,
}

/// Render the tab bar as a single `Line` of styled spans.
///
/// Hidden when there is only one tab (backward-compatible single-room mode).
/// Active tab is highlighted; inactive tabs with unread messages show a count badge.
pub(super) fn render_tab_bar(tabs: &[TabInfo]) -> Option<Line<'static>> {
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

/// Look up a username's color from the map, falling back to the hash-based
/// palette index if the user has not been assigned a color yet.
pub(super) fn user_color(username: &str, color_map: &ColorMap) -> Color {
    if let Some(&color) = color_map.get(username) {
        return color;
    }
    let hash = username.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as usize)
    });
    PALETTE[hash % PALETTE.len()]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── wrap_words tests ──────────────────────────────────────────────────────

    #[test]
    fn wrap_words_preserves_explicit_newline() {
        let chunks = wrap_words("hello\nworld", 40);
        assert_eq!(chunks, ["hello", "world"]);
    }

    #[test]
    fn wrap_words_double_newline_produces_blank_line() {
        let chunks = wrap_words("first\n\nsecond", 40);
        assert_eq!(chunks, ["first", "", "second"]);
    }

    #[test]
    fn wrap_words_newline_with_wrapping() {
        // Each logical line is word-wrapped independently.
        let chunks = wrap_words("short\na b c d e", 5);
        // "short" fits in 5 chars; "a b c d e" wraps as "a b c", "d e"
        assert_eq!(chunks, ["short", "a b c", "d e"]);
    }

    #[test]
    fn wrap_words_no_newline_unchanged() {
        let chunks = wrap_words("hello world", 40);
        assert_eq!(chunks, ["hello world"]);
    }

    #[test]
    fn wrap_words_trailing_newline_produces_blank_chunk() {
        let chunks = wrap_words("hello\n", 40);
        assert_eq!(chunks, ["hello", ""]);
    }

    // ── render_content_with_mentions ─────────────────────────────────────────

    #[test]
    fn render_mentions_no_at_returns_single_raw_span() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("hello world", &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world");
    }

    #[test]
    fn render_mentions_bare_at_no_username_is_literal() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("email@", &cm);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "email@");
    }

    #[test]
    fn render_mentions_single_mention() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("hey @alice!", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hey ");
        assert_eq!(spans[1].content, "@alice");
        assert_eq!(spans[2].content, "!");
    }

    #[test]
    fn render_mentions_multiple_mentions() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("@alice and @bob", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "@alice");
        assert_eq!(spans[1].content, " and ");
        assert_eq!(spans[2].content, "@bob");
    }

    #[test]
    fn render_mentions_mention_only() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("@r2d2", &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "@r2d2");
    }

    #[test]
    fn render_mentions_empty_content() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("", &cm);
        assert!(!spans.is_empty());
    }

    // ── assign_color ─────────────────────────────────────────────────────────

    #[test]
    fn assign_color_returns_consistent_color() {
        let mut cm = ColorMap::new();
        let c1 = assign_color("alice", &mut cm);
        let c2 = assign_color("alice", &mut cm);
        assert_eq!(c1, c2);
    }

    #[test]
    fn assign_color_different_users_get_different_colors() {
        let mut cm = ColorMap::new();
        let c1 = assign_color("alice", &mut cm);
        let c2 = assign_color("bob", &mut cm);
        assert_ne!(c1, c2);
    }

    #[test]
    fn assign_color_avoids_collision_when_preferred_taken() {
        let mut cm = ColorMap::new();
        // "alice" gets her preferred color first.
        let alice_color = assign_color("alice", &mut cm);
        // Find another username that hashes to the same palette index.
        let mut collider = String::new();
        for i in 0u32..10_000 {
            let name = format!("u{i}");
            let hash = name.bytes().fold(0usize, |acc, b| {
                acc.wrapping_mul(31).wrapping_add(b as usize)
            });
            if PALETTE[hash % PALETTE.len()] == alice_color {
                collider = name;
                break;
            }
        }
        assert!(!collider.is_empty(), "could not find a colliding username");
        let collider_color = assign_color(&collider, &mut cm);
        // The collider should NOT get Alice's color — it should get a different unused one.
        assert_ne!(collider_color, alice_color);
    }

    #[test]
    fn assign_color_fills_all_palette_slots() {
        let mut cm = ColorMap::new();
        let mut colors = HashSet::new();
        // Assign colors to enough users to fill the palette.
        for i in 0..PALETTE.len() {
            let c = assign_color(&format!("user{i}"), &mut cm);
            colors.insert(c);
        }
        // Every palette color should be used exactly once.
        assert_eq!(colors.len(), PALETTE.len());
    }

    #[test]
    fn assign_color_accepts_collision_when_palette_exhausted() {
        let mut cm = ColorMap::new();
        // Fill all palette slots.
        for i in 0..PALETTE.len() {
            assign_color(&format!("user{i}"), &mut cm);
        }
        // The 11th user must accept a collision.
        let c = assign_color("overflow", &mut cm);
        assert!(PALETTE.contains(&c));
    }

    #[test]
    fn user_color_uses_map_when_present() {
        let mut cm = ColorMap::new();
        cm.insert("alice".to_owned(), Color::LightRed);
        assert_eq!(user_color("alice", &cm), Color::LightRed);
    }

    #[test]
    fn user_color_falls_back_to_hash_when_not_in_map() {
        let cm = ColorMap::new();
        let c = user_color("alice", &cm);
        let hash = "alice".bytes().fold(0usize, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(b as usize)
        });
        assert_eq!(c, PALETTE[hash % PALETTE.len()]);
    }

    // ── find_view_start tests ────────────────────────────────────────────────

    #[test]
    fn find_view_start_single_line_messages() {
        // 5 messages, each 1 line tall
        let heights = [1, 1, 1, 1, 1];
        assert_eq!(find_view_start(&heights, 0), (0, 0));
        assert_eq!(find_view_start(&heights, 2), (2, 0));
        assert_eq!(find_view_start(&heights, 4), (4, 0));
    }

    #[test]
    fn find_view_start_multi_line_message_partial() {
        // msg0: 1 line, msg1: 5 lines, msg2: 1 line = 7 total
        let heights = [1, 5, 1];
        // view_top=0 → start at msg0, skip 0
        assert_eq!(find_view_start(&heights, 0), (0, 0));
        // view_top=1 → start at msg1 (line 1 is first line of msg1), skip 0
        assert_eq!(find_view_start(&heights, 1), (1, 0));
        // view_top=2 → start at msg1, skip 1 line (show lines 2-5 of msg1)
        assert_eq!(find_view_start(&heights, 2), (1, 1));
        // view_top=3 → start at msg1, skip 2 lines
        assert_eq!(find_view_start(&heights, 3), (1, 2));
        // view_top=5 → start at msg1, skip 4 lines (show only last line of msg1)
        assert_eq!(find_view_start(&heights, 5), (1, 4));
        // view_top=6 → start at msg2, skip 0
        assert_eq!(find_view_start(&heights, 6), (2, 0));
    }

    #[test]
    fn find_view_start_past_end_returns_len() {
        let heights = [1, 2];
        // view_top=3 is exactly total_lines → past end
        assert_eq!(find_view_start(&heights, 3), (2, 0));
        // view_top=10 → way past end
        assert_eq!(find_view_start(&heights, 10), (2, 0));
    }

    #[test]
    fn find_view_start_empty_heights() {
        let heights: [usize; 0] = [];
        assert_eq!(find_view_start(&heights, 0), (0, 0));
    }

    // ── welcome_splash tests ──────────────────────────────────────────────

    #[test]
    fn welcome_splash_contains_tagline() {
        let text = welcome_splash(0, 60);
        let content: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("");
        assert!(
            content.contains("Agent coordination for humans"),
            "splash should contain tagline"
        );
    }

    #[test]
    fn welcome_splash_contains_version() {
        let text = welcome_splash(0, 60);
        let content: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("");
        let version = env!("CARGO_PKG_VERSION");
        assert!(
            content.contains(&format!("v{version}")),
            "splash should contain version"
        );
    }

    #[test]
    fn welcome_splash_contains_date() {
        let text = welcome_splash(0, 60);
        let content: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("");
        let today = Local::now().format("%Y-%m-%d").to_string();
        assert!(
            content.contains(&today),
            "splash should contain today's date"
        );
    }

    #[test]
    fn welcome_splash_animation_changes_between_phases() {
        let frame_a = welcome_splash(0, 60);
        let frame_b = welcome_splash(10, 60); // one BLINK_INTERVAL later
        let content_a: String = frame_a
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("");
        let content_b: String = frame_b
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("");
        assert_ne!(
            content_a, content_b,
            "animation should change between phases"
        );
    }

    #[test]
    fn welcome_splash_has_more_lines_than_logo() {
        let text = welcome_splash(0, 60);
        // Logo is 7 lines + 1 vpad + 1 blank separator + tagline + version/date = 12 min
        assert!(
            text.lines.len() >= 11,
            "splash should have logo + tagline + version lines, got {}",
            text.lines.len()
        );
    }

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

    #[test]
    fn find_view_start_scroll_through_tall_message_line_by_line() {
        // Simulate scrolling through a 10-line message with a 3-line viewport
        let heights = [2, 10, 2]; // total = 14
        let viewport = 3;

        // scroll_offset goes from 0 to total-viewport = 11
        // For each offset, verify the view start advances line by line
        let expected: Vec<(usize, usize)> = vec![
            // scroll_offset=0: vb=14, vt=11 → msg1 skip 9 (last line of 10-line msg)
            (1, 9),
            // scroll_offset=1: vb=13, vt=10 → msg1 skip 8
            (1, 8),
            // scroll_offset=2: vb=12, vt=9 → msg1 skip 7
            (1, 7),
            // scroll_offset=3: vb=11, vt=8 → msg1 skip 6
            (1, 6),
            // scroll_offset=4: vb=10, vt=7 → msg1 skip 5
            (1, 5),
            // scroll_offset=5: vb=9, vt=6 → msg1 skip 4
            (1, 4),
            // scroll_offset=6: vb=8, vt=5 → msg1 skip 3
            (1, 3),
            // scroll_offset=7: vb=7, vt=4 → msg1 skip 2
            (1, 2),
            // scroll_offset=8: vb=6, vt=3 → msg1 skip 1
            (1, 1),
            // scroll_offset=9: vb=5, vt=2 → msg1 skip 0
            (1, 0),
            // scroll_offset=10: vb=4, vt=1 → msg0 skip 1
            (0, 1),
            // scroll_offset=11: vb=3, vt=0 → msg0 skip 0
            (0, 0),
        ];

        let total_lines: usize = heights.iter().sum();
        for (offset, &(exp_msg, exp_skip)) in expected.iter().enumerate() {
            let view_bottom = total_lines.saturating_sub(offset);
            let view_top = view_bottom.saturating_sub(viewport);
            let result = find_view_start(&heights, view_top);
            assert_eq!(
                result,
                (exp_msg, exp_skip),
                "scroll_offset={offset}, view_top={view_top}"
            );
        }
    }
}
