//! Message formatting and layout helpers for the TUI.
//!
//! Formats `Message` variants into styled ratatui `Text` for display in the
//! message pane, handles word wrapping, and provides scroll viewport helpers.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

use crate::message::Message;

// Re-export submodules so existing `use render::*` in mod.rs keeps working.
pub(super) use super::colors::{assign_color, user_color, ColorMap};
pub(super) use super::markdown::render_chunk_content;
pub(super) use super::panel::{
    build_member_panel_spans, ellipsize_status, member_panel_row_width, render_tab_bar, TabInfo,
};
pub(super) use super::render_bots::welcome_splash;

/// Arrow glyph used in DM display (`→`).
const DM_ARROW: &str = "\u{2192}";

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
        Message::Event {
            ts,
            event_type,
            content,
            ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            let tag = format!("[event:{event_type}]");
            let prefix_plain = format!("[{ts_str}] {tag} ");
            let prefix_width = prefix_plain.chars().count();
            let content_width = available_width.saturating_sub(prefix_width);
            let chunks = wrap_words(content, content_width);
            let indent = " ".repeat(prefix_width);
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{tag} {chunk}"), Style::default().fg(Color::Yellow)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw(indent.clone()),
                        Span::styled(chunk, Style::default().fg(Color::Yellow)),
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

/// Compute per-message visible line counts for a viewport window.
///
/// Given message `heights`, the viewport `height`, and the `view_top` line
/// index, returns a vec of `(message_index, visible_line_count)` pairs for
/// every message that has at least one line inside the viewport. Messages
/// partially clipped at the top or bottom are included with only their
/// visible line count.
#[cfg(test)]
fn visible_line_counts(
    heights: &[usize],
    view_top: usize,
    viewport_height: usize,
) -> Vec<(usize, usize)> {
    let (start_idx, skip_first) = find_view_start(heights, view_top);
    let mut result = Vec::new();
    let mut remaining = viewport_height;
    for (i, &h) in heights[start_idx..].iter().enumerate() {
        if remaining == 0 {
            break;
        }
        let lines = if i == 0 { h - skip_first } else { h };
        let visible = lines.min(remaining);
        result.push((start_idx + i, visible));
        remaining -= visible;
    }
    result
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

    // ── visible_line_counts tests ─────────────────────────────────────────

    #[test]
    fn visible_line_counts_all_fit() {
        // 3 messages, each 1 line, viewport of 5 — all fit fully.
        let counts = visible_line_counts(&[1, 1, 1], 0, 5);
        assert_eq!(counts, vec![(0, 1), (1, 1), (2, 1)]);
    }

    #[test]
    fn visible_line_counts_bottom_truncation() {
        // 3 messages of heights [2, 5, 3], total=10, viewport=4, view_top=0.
        // msg0 fully visible (2 lines), msg1 partially visible (2 of 5 lines).
        let counts = visible_line_counts(&[2, 5, 3], 0, 4);
        assert_eq!(counts, vec![(0, 2), (1, 2)]);
    }

    #[test]
    fn visible_line_counts_top_skip_and_bottom_truncation() {
        // heights [3, 4, 2], total=9, viewport=3, view_top=2.
        // msg0: 3 lines, skip 2 → 1 visible line.
        // msg1: 4 lines, only 2 fit → 2 visible lines.
        let counts = visible_line_counts(&[3, 4, 2], 2, 3);
        assert_eq!(counts, vec![(0, 1), (1, 2)]);
    }

    #[test]
    fn visible_line_counts_single_tall_message() {
        // One 10-line message, viewport=3, view_top=4.
        // Skip 4 lines, show 3 of remaining 6.
        let counts = visible_line_counts(&[10], 4, 3);
        assert_eq!(counts, vec![(0, 3)]);
    }

    #[test]
    fn visible_line_counts_exact_fit() {
        // heights [2, 3], viewport=5, view_top=0 — exactly fills viewport.
        let counts = visible_line_counts(&[2, 3], 0, 5);
        assert_eq!(counts, vec![(0, 2), (1, 3)]);
    }

    #[test]
    fn visible_line_counts_empty() {
        let counts = visible_line_counts(&[], 0, 5);
        assert_eq!(counts, vec![]);
    }

    #[test]
    fn visible_line_counts_zero_viewport() {
        let counts = visible_line_counts(&[3, 2], 0, 0);
        assert_eq!(counts, vec![]);
    }
}
