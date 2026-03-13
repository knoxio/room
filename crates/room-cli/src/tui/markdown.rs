//! Inline markdown parser for TUI message rendering.
//!
//! Single-pass parser that converts markdown syntax (`***bold+italic***`,
//! `**bold**`, `*italic*`, `` `code` ``, `@mention`) into styled ratatui
//! `Span`s. Also handles triple-backtick code block fencing.

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

use super::colors::{user_color, ColorMap};

/// Split a message content string into styled spans, rendering `@username`
/// mentions and inline markdown (`***bold+italic***`, `**bold**`, `*italic*`,
/// `` `code` ``).
///
/// Single-pass parser. `***` is matched before `**` to support bold+italic.
/// Bold content (`**...**`) is recursively parsed for `@mentions` and
/// `*italic*` nesting. Other delimiters are not nested.
pub(crate) fn render_content_with_mentions(
    content: &str,
    color_map: &ColorMap,
) -> Vec<Span<'static>> {
    render_inline_markdown(content, color_map, Style::default(), false)
}

/// Parameterized inline markdown parser shared by top-level and nested contexts.
///
/// When `nested` is false, handles all inline markdown: `***bold+italic***`,
/// `**bold**`, `*italic*`, `` `code` ``, and `@mentions`.
/// When `nested` is true (inside `**...**`), only handles `*italic*` and
/// `@mentions` — avoids re-parsing outer delimiters.
///
/// `base_style` is applied to plain text and merged into delimiter styles.
fn render_inline_markdown(
    content: &str,
    color_map: &ColorMap,
    base_style: Style,
    nested: bool,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut plain_start = 0;

    while i < len {
        // `***bold+italic***` — top-level only
        if !nested
            && i + 2 < len
            && bytes[i] == b'*'
            && bytes[i + 1] == b'*'
            && bytes[i + 2] == b'*'
        {
            if let Some(close) = find_closing_triple_star(bytes, i + 3) {
                if close > i + 3 {
                    flush_styled(content, plain_start, i, base_style, &mut spans);
                    spans.push(Span::styled(
                        content[i + 3..close].to_string(),
                        base_style.add_modifier(Modifier::BOLD | Modifier::ITALIC),
                    ));
                    i = close + 3;
                    plain_start = i;
                    continue;
                }
            }
        }

        // `**bold**` — top-level only, delegates to nested call
        if !nested && i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            if let Some(close) = find_closing_double_star(bytes, i + 2) {
                if close > i + 2 {
                    flush_styled(content, plain_start, i, base_style, &mut spans);
                    spans.extend(render_inline_markdown(
                        &content[i + 2..close],
                        color_map,
                        base_style.add_modifier(Modifier::BOLD),
                        true,
                    ));
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
                    flush_styled(content, plain_start, i, base_style, &mut spans);
                    spans.push(Span::styled(
                        content[i + 1..close].to_string(),
                        base_style.add_modifier(Modifier::ITALIC),
                    ));
                    i = close + 1;
                    plain_start = i;
                    continue;
                }
            }
        }

        // `` `code` `` — top-level only
        if !nested && bytes[i] == b'`' {
            if let Some(close) = find_closing_backtick(bytes, i + 1) {
                if close > i + 1 {
                    flush_styled(content, plain_start, i, base_style, &mut spans);
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
                flush_styled(content, plain_start, i, base_style, &mut spans);
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

    flush_styled(content, plain_start, len, base_style, &mut spans);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }
    spans
}

/// Flush accumulated plain text from `content[start..end]` into `spans`.
fn flush_styled(
    content: &str,
    start: usize,
    end: usize,
    style: Style,
    spans: &mut Vec<Span<'static>>,
) {
    if start < end {
        spans.push(Span::styled(content[start..end].to_string(), style));
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

/// Render a single chunk with code-block fence awareness.
///
/// Tracks whether we are inside a triple-backtick code block via
/// `in_code_block`. Fence lines are rendered dimmed; lines inside a code
/// block are rendered in code color (yellow) without markdown parsing.
pub(crate) fn render_chunk_content(
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
}
