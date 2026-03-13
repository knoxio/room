use std::collections::{HashMap, HashSet};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

use room_protocol::SubscriptionTier;

use crate::message::Message;

pub(super) use super::render_bots::welcome_splash;

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

/// Split a message content string into styled spans, rendering `@username`
/// mentions and inline markdown (`***bold+italic***`, `**bold**`, `*italic*`,
/// `` `code` ``).
///
/// Single-pass parser. `***` is matched before `**` to support bold+italic.
/// Bold content (`**...**`) is recursively parsed for `@mentions` and
/// `*italic*` nesting. Other delimiters are not nested.
pub(super) fn render_content_with_mentions(
    content: &str,
    color_map: &ColorMap,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut plain_start = 0;

    while i < len {
        // `***bold+italic***`
        if i + 2 < len && bytes[i] == b'*' && bytes[i + 1] == b'*' && bytes[i + 2] == b'*' {
            if let Some(close) = find_closing_triple_star(bytes, i + 3) {
                if close > i + 3 {
                    flush_plain(content, plain_start, i, &mut spans);
                    spans.push(Span::styled(
                        content[i + 3..close].to_string(),
                        Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC),
                    ));
                    i = close + 3;
                    plain_start = i;
                    continue;
                }
            }
        }

        // `**bold**` — inner content parsed for @mentions and *italic*
        if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            if let Some(close) = find_closing_double_star(bytes, i + 2) {
                if close > i + 2 {
                    flush_plain(content, plain_start, i, &mut spans);
                    spans.extend(render_bold_inner(&content[i + 2..close], color_map));
                    i = close + 2;
                    plain_start = i;
                    continue;
                }
            }
        }

        // `*italic*` — only when not part of `**`
        if bytes[i] == b'*' && !(i + 1 < len && bytes[i + 1] == b'*') {
            if let Some(close) = find_closing_single_star(bytes, i + 1) {
                if close > i + 1 {
                    flush_plain(content, plain_start, i, &mut spans);
                    spans.push(Span::styled(
                        content[i + 1..close].to_string(),
                        Style::default().add_modifier(Modifier::ITALIC),
                    ));
                    i = close + 1;
                    plain_start = i;
                    continue;
                }
            }
        }

        // `` `code` ``
        if bytes[i] == b'`' {
            if let Some(close) = find_closing_backtick(bytes, i + 1) {
                if close > i + 1 {
                    flush_plain(content, plain_start, i, &mut spans);
                    spans.push(Span::styled(
                        content[i + 1..close].to_string(),
                        Style::default().fg(Color::Yellow),
                    ));
                    i = close + 1;
                    plain_start = i;
                    continue;
                }
            }
        }

        // `@mention`
        if bytes[i] == b'@' {
            let after_at = &content[i + 1..];
            let username_end = after_at
                .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                .unwrap_or(after_at.len());
            if username_end > 0 {
                flush_plain(content, plain_start, i, &mut spans);
                let username = &after_at[..username_end];
                spans.push(Span::styled(
                    format!("@{username}"),
                    Style::default()
                        .fg(user_color(username, color_map))
                        .add_modifier(Modifier::BOLD),
                ));
                i += 1 + username_end;
                plain_start = i;
                continue;
            }
        }

        i += 1;
    }

    flush_plain(content, plain_start, len, &mut spans);
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

/// Flush accumulated plain text from `content[start..end]` into `spans`.
fn flush_plain(content: &str, start: usize, end: usize, spans: &mut Vec<Span<'static>>) {
    if start < end {
        spans.push(Span::raw(content[start..end].to_string()));
    }
}

/// Find the byte position of the closing `**` starting from `start`.
fn find_closing_double_star(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut j = start;
    while j + 1 < len {
        if bytes[j] == b'*' && bytes[j + 1] == b'*' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Find the byte position of a closing single `*`, skipping any `**` sequences.
fn find_closing_single_star(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut j = start;
    while j < len {
        if bytes[j] == b'*' {
            if j + 1 >= len || bytes[j + 1] != b'*' {
                return Some(j);
            }
            // Skip `**`
            j += 2;
            continue;
        }
        j += 1;
    }
    None
}

/// Find the byte position of the closing backtick starting from `start`.
fn find_closing_backtick(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..]
        .iter()
        .position(|&b| b == b'`')
        .map(|p| start + p)
}

/// Find the byte position of the closing `***` starting from `start`.
fn find_closing_triple_star(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut j = start;
    while j + 2 < len {
        if bytes[j] == b'*' && bytes[j + 1] == b'*' && bytes[j + 2] == b'*' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Parse content inside `**...**` for nested `@mentions` and `*italic*`.
///
/// Every span produced carries [`Modifier::BOLD`] since it sits inside a bold
/// delimiter. `@mentions` keep their user color; `*italic*` segments get
/// `BOLD | ITALIC`.
fn render_bold_inner(content: &str, color_map: &ColorMap) -> Vec<Span<'static>> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut plain_start = 0;

    while i < len {
        // `*italic*` inside bold → BOLD|ITALIC
        if bytes[i] == b'*' && !(i + 1 < len && bytes[i + 1] == b'*') {
            if let Some(close) = find_closing_single_star(bytes, i + 1) {
                if close > i + 1 {
                    if plain_start < i {
                        spans.push(Span::styled(content[plain_start..i].to_string(), bold));
                    }
                    spans.push(Span::styled(
                        content[i + 1..close].to_string(),
                        Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC),
                    ));
                    i = close + 1;
                    plain_start = i;
                    continue;
                }
            }
        }

        // `@mention` inside bold
        if bytes[i] == b'@' {
            let after_at = &content[i + 1..];
            let username_end = after_at
                .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                .unwrap_or(after_at.len());
            if username_end > 0 {
                if plain_start < i {
                    spans.push(Span::styled(content[plain_start..i].to_string(), bold));
                }
                let username = &after_at[..username_end];
                spans.push(Span::styled(
                    format!("@{username}"),
                    Style::default()
                        .fg(user_color(username, color_map))
                        .add_modifier(Modifier::BOLD),
                ));
                i += 1 + username_end;
                plain_start = i;
                continue;
            }
        }

        i += 1;
    }

    if plain_start < len {
        spans.push(Span::styled(content[plain_start..len].to_string(), bold));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), bold));
    }
    spans
}

/// Render a single chunk with code-block fence awareness.
///
/// Tracks whether we are inside a triple-backtick code block via
/// `in_code_block`. Fence lines are rendered dimmed; lines inside a code
/// block are rendered in code color (yellow) without markdown parsing.
fn render_chunk_content(
    chunk: &str,
    in_code_block: &mut bool,
    color_map: &ColorMap,
) -> Vec<Span<'static>> {
    let is_fence = chunk.starts_with("```");
    if is_fence {
        *in_code_block = !*in_code_block;
        return vec![Span::styled(
            chunk.to_string(),
            Style::default().fg(Color::DarkGray),
        )];
    }
    if *in_code_block {
        return vec![Span::styled(
            chunk.to_string(),
            Style::default().fg(Color::Yellow),
        )];
    }
    render_content_with_mentions(chunk, color_map)
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
            let mut in_code_block = false;
            for (i, chunk) in chunks.into_iter().enumerate() {
                let content_spans = render_chunk_content(&chunk, &mut in_code_block, color_map);
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
                    line_spans.extend(content_spans);
                    lines.push(Line::from(line_spans));
                } else {
                    let mut line_spans = vec![Span::raw(indent.clone())];
                    line_spans.extend(content_spans);
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
            let mut in_code_block = false;
            for (i, chunk) in chunks.into_iter().enumerate() {
                let content_spans = render_chunk_content(&chunk, &mut in_code_block, color_map);
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
                    line_spans.extend(content_spans);
                    lines.push(Line::from(line_spans));
                } else {
                    let mut line_spans = vec![Span::raw(indent.clone())];
                    line_spans.extend(content_spans);
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

/// Build the styled spans for a single member panel row.
///
/// Renders: ` <username>` (bold, colored) + optional tier indicator + optional
/// status (dimmed) + trailing space. Used by the floating member panel.
pub(super) fn build_member_panel_spans(
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
pub(super) fn member_panel_row_width(
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
pub(super) fn ellipsize_status(status: &str, max_chars: usize) -> String {
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

    // ── render_content_with_mentions: markdown formatting ───────────────────

    #[test]
    fn render_bold_text() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("hello **world**!", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert_eq!(spans[1].content, "world");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].content, "!");
    }

    #[test]
    fn render_italic_text() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("hello *world*!", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert_eq!(spans[1].content, "world");
        assert!(spans[1].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[2].content, "!");
    }

    #[test]
    fn render_code_text() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("run `cargo test` now", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "run ");
        assert_eq!(spans[1].content, "cargo test");
        assert_eq!(spans[1].style.fg, Some(Color::Yellow));
        assert_eq!(spans[2].content, " now");
    }

    #[test]
    fn render_bold_and_mention_together() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("**important** @alice check this", &cm);
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].content, "important");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content, " ");
        assert_eq!(spans[2].content, "@alice");
        assert_eq!(spans[3].content, " check this");
    }

    #[test]
    fn render_unclosed_bold_is_literal() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("hello **world", &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello **world");
    }

    #[test]
    fn render_unclosed_backtick_is_literal() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("hello `world", &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello `world");
    }

    #[test]
    fn render_empty_delimiters_are_literal() {
        let cm = ColorMap::new();
        // Empty bold: `****` — no content between delimiters
        let spans = render_content_with_mentions("a**b", &cm);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "a**b");

        // Empty backticks: `` `` ``
        let spans = render_content_with_mentions("a``b", &cm);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "a``b");
    }

    #[test]
    fn render_multiple_formatted_spans() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("**bold** and `code` and *italic*", &cm);
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0].content, "bold");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content, " and ");
        assert_eq!(spans[2].content, "code");
        assert_eq!(spans[2].style.fg, Some(Color::Yellow));
        assert_eq!(spans[3].content, " and ");
        assert_eq!(spans[4].content, "italic");
        assert!(spans[4].style.add_modifier.contains(Modifier::ITALIC));
    }

    // ── bold+italic (***) ──────────────────────────────────────────────────

    #[test]
    fn render_bold_italic_text() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("hello ***world***!", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert_eq!(spans[1].content, "world");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(spans[1].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[2].content, "!");
    }

    #[test]
    fn render_bold_italic_only() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("***emphasis***", &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "emphasis");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(spans[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn render_unclosed_triple_star_falls_through_to_bold() {
        let cm = ColorMap::new();
        // No closing ***, so falls through to ** handler
        let spans = render_content_with_mentions("***text**end", &cm);
        // ** matches at position 0 (first two stars), finds closing ** at "text**"
        // Content between: "*text"
        assert_eq!(spans[0].content, "*text");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn render_triple_star_empty_content_is_literal() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("******", &cm);
        // *** followed by *** — but content is empty (close == i+3, not > i+3)
        // Falls through: ** matches, ** finds closing, content is "**" → bold
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "******");
    }

    // ── nested @mention inside bold ─────────────────────────────────────────

    #[test]
    fn render_bold_with_nested_mention() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("**hello @alice!**", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content, "@alice");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].content, "!");
        assert!(spans[2].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn render_bold_with_mention_only() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("**@bob**", &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "@bob");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn render_bold_with_multiple_mentions() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("**@alice and @bob**", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "@alice");
        assert_eq!(spans[1].content, " and ");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].content, "@bob");
    }

    // ── nested italic inside bold ───────────────────────────────────────────

    #[test]
    fn render_bold_with_nested_italic() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("**text *emphasis* more**", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "text ");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(!spans[0].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[1].content, "emphasis");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(spans[1].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[2].content, " more");
        assert!(spans[2].style.add_modifier.contains(Modifier::BOLD));
        assert!(!spans[2].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn render_bold_with_italic_and_mention() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("**@alice said *important* stuff**", &cm);
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].content, "@alice");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content, " said ");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].content, "important");
        assert!(spans[2].style.add_modifier.contains(Modifier::BOLD));
        assert!(spans[2].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[3].content, " stuff");
        assert!(spans[3].style.add_modifier.contains(Modifier::BOLD));
    }

    // ── code block fencing ──────────────────────────────────────────────────

    #[test]
    fn render_chunk_content_fence_toggles_code_block() {
        let cm = ColorMap::new();
        let mut in_code = false;

        // Opening fence
        let spans = render_chunk_content("```", &mut in_code, &cm);
        assert!(in_code);
        assert_eq!(spans[0].style.fg, Some(Color::DarkGray));

        // Content inside code block
        let spans = render_chunk_content("let x = 42;", &mut in_code, &cm);
        assert_eq!(spans[0].content, "let x = 42;");
        assert_eq!(spans[0].style.fg, Some(Color::Yellow));

        // Closing fence
        let spans = render_chunk_content("```", &mut in_code, &cm);
        assert!(!in_code);
        assert_eq!(spans[0].style.fg, Some(Color::DarkGray));

        // After code block — normal markdown
        let spans = render_chunk_content("**bold**", &mut in_code, &cm);
        assert_eq!(spans[0].content, "bold");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn render_chunk_content_code_block_suppresses_markdown() {
        let cm = ColorMap::new();
        let mut in_code = true; // already inside a code block

        // Bold syntax inside code block is NOT parsed
        let spans = render_chunk_content("**not bold**", &mut in_code, &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "**not bold**");
        assert_eq!(spans[0].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn render_chunk_content_fence_with_language_tag() {
        let cm = ColorMap::new();
        let mut in_code = false;

        let spans = render_chunk_content("```rust", &mut in_code, &cm);
        assert!(in_code, "fence with language tag should toggle code block");
        assert_eq!(spans[0].content, "```rust");
        assert_eq!(spans[0].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn render_chunk_content_normal_backticks_not_fence() {
        let cm = ColorMap::new();
        let mut in_code = false;

        // Single backtick — not a fence
        let spans = render_chunk_content("`code`", &mut in_code, &cm);
        assert!(!in_code);
        assert_eq!(spans[0].content, "code");
        assert_eq!(spans[0].style.fg, Some(Color::Yellow));
    }

    // ── backtick edge cases ─────────────────────────────────────────────────

    #[test]
    fn render_backtick_at_end_of_line() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("use `fmt`", &cm);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "use ");
        assert_eq!(spans[1].content, "fmt");
        assert_eq!(spans[1].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn render_backtick_adjacent_to_bold() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("`code`**bold**", &cm);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "code");
        assert_eq!(spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(spans[1].content, "bold");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn render_backtick_preserves_inner_stars() {
        let cm = ColorMap::new();
        // Stars inside backticks are literal
        let spans = render_content_with_mentions("`**not bold**`", &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "**not bold**");
        assert_eq!(spans[0].style.fg, Some(Color::Yellow));
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
