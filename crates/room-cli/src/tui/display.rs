use unicode_width::UnicodeWidthChar;

/// Wrap input text for display in the input box.
///
/// Splits on `\n` (explicit newlines from Shift+Enter), then wraps each
/// logical line character-by-character at `width` display columns using
/// Unicode display widths. Returns the flat list of display rows.
///
/// If `width` is 0, returns each logical line unsplit.
pub(super) fn wrap_input_display(input: &str, width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for logical_line in input.split('\n') {
        if width == 0 {
            rows.push(logical_line.to_string());
            continue;
        }
        let mut current = String::new();
        let mut col: usize = 0;
        for ch in logical_line.chars() {
            let w = ch.width().unwrap_or(0);
            // Wrap before adding if the character would overflow and we have content.
            if col + w > width && col > 0 {
                rows.push(std::mem::take(&mut current));
                col = 0;
            }
            current.push(ch);
            col += w;
        }
        rows.push(current);
    }
    // Always at least one row.
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Return the `(display_row, display_col)` of `cursor_pos` (a byte index into
/// `input`) after applying the same wrapping logic as [`wrap_input_display`].
pub(super) fn cursor_display_pos(input: &str, cursor_pos: usize, width: usize) -> (usize, usize) {
    let mut row: usize = 0;
    let mut col: usize = 0;
    for (i, ch) in input.char_indices() {
        if i >= cursor_pos {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            let w = ch.width().unwrap_or(0);
            if width > 0 && col + w > width && col > 0 {
                row += 1;
                col = 0;
            }
            col += w;
        }
    }
    // If col == width and there is remaining content that is not a newline,
    // the next character would start on a new row — advance the cursor there
    // so it doesn't render past the right border of the input box.
    if width > 0
        && col == width
        && cursor_pos < input.len()
        && !input[cursor_pos..].starts_with('\n')
    {
        row += 1;
        col = 0;
    }
    (row, col)
}

/// Given a target `(display_row, display_col)` in the wrapped display of `input`,
/// return the byte offset into `input` that corresponds to that position.
///
/// Uses the same wrapping logic as [`wrap_input_display`] and [`cursor_display_pos`].
/// If `target_col` is beyond the end of `target_row`, the returned offset is
/// clamped to the last character position in that row (before any soft-wrap or newline).
pub(super) fn byte_offset_at_display_pos(
    input: &str,
    target_row: usize,
    target_col: usize,
    width: usize,
) -> usize {
    let mut row: usize = 0;
    let mut col: usize = 0;

    for (i, ch) in input.char_indices() {
        if row == target_row {
            if col >= target_col {
                return i;
            }
            // About to advance past the target row — clamp before the newline.
            if ch == '\n' {
                return i;
            }
        }

        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            let w = ch.width().unwrap_or(0);
            if width > 0 && col + w > width && col > 0 {
                // This character starts a new soft-wrapped row.
                if row == target_row {
                    // target_col was beyond the end of the soft-wrapped row.
                    return i;
                }
                row += 1;
                col = 0;
                if row == target_row && col >= target_col {
                    return i;
                }
            }
            col += w;
        }
    }

    input.len()
}

/// If the cursor is currently within a `@mention` context — i.e. there is an
/// `@` before `cursor_pos` with no space or newline between them — return the
/// byte index of the `@` and the query string typed after it.
pub(super) fn find_at_context(input: &str, cursor_pos: usize) -> Option<(usize, &str)> {
    let before = &input[..cursor_pos];
    let at_pos = before.rfind('@')?;
    let query = &before[at_pos + 1..];
    if query.contains([' ', '\n']) {
        return None;
    }
    Some((at_pos, query))
}

/// Move the cursor to the start of the previous word.
///
/// "Word" is a maximal run of non-whitespace characters. Starting from
/// `cursor_pos`, the function first skips any trailing whitespace going
/// backwards (phase 1), then skips the preceding non-whitespace run (phase 2),
/// landing at the first byte of that word.
///
/// Returns 0 if there is no previous word.
pub(super) fn prev_word_start(input: &str, cursor_pos: usize) -> usize {
    let before = &input[..cursor_pos];
    let mut it = before.char_indices().rev();
    // Phase 1: skip trailing whitespace going backward.
    loop {
        match it.next() {
            None => return 0,
            Some((_, c)) if c.is_whitespace() => continue,
            Some((i, _)) => {
                // Found the first non-whitespace; now skip the whole word.
                // `i` is the byte index of the last char of the preceding word.
                let mut word_start = i;
                for (j, c) in it {
                    if c.is_whitespace() {
                        // `j` is the byte index of the whitespace char before the word.
                        return j + c.len_utf8();
                    }
                    word_start = j;
                }
                return word_start;
            }
        }
    }
}

/// Move the cursor to one past the end of the next word.
///
/// "Word" is a maximal run of non-whitespace characters. Starting from
/// `cursor_pos`, the function first skips any leading whitespace going forward
/// (phase 1), then skips the next non-whitespace run (phase 2), landing at the
/// byte just after the last character of that word.
///
/// Returns `input.len()` if there is no next word.
pub(super) fn next_word_end(input: &str, cursor_pos: usize) -> usize {
    let after = &input[cursor_pos..];
    let mut it = after.char_indices().peekable();
    // Phase 1: skip leading whitespace.
    loop {
        match it.next() {
            None => return input.len(),
            Some((_, c)) if c.is_whitespace() => continue,
            Some(_) => break,
        }
    }
    // Phase 2: skip non-whitespace.
    loop {
        match it.next() {
            None => return input.len(),
            Some((i, c)) if c.is_whitespace() => return cursor_pos + i,
            _ => continue,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── wrap_input_display ──────────────────────────────────────────────────

    #[test]
    fn wrap_empty_string_returns_one_empty_row() {
        assert_eq!(wrap_input_display("", 10), vec![""]);
    }

    #[test]
    fn wrap_fits_on_one_line() {
        assert_eq!(wrap_input_display("hello", 10), vec!["hello"]);
    }

    #[test]
    fn wrap_exactly_at_boundary_stays_one_line() {
        assert_eq!(wrap_input_display("abcde", 5), vec!["abcde"]);
    }

    #[test]
    fn wrap_one_char_over_splits_to_two_rows() {
        assert_eq!(wrap_input_display("abcdef", 5), vec!["abcde", "f"]);
    }

    #[test]
    fn wrap_explicit_newline_splits_logical_lines() {
        assert_eq!(
            wrap_input_display("hello\nworld", 20),
            vec!["hello", "world"]
        );
    }

    #[test]
    fn wrap_explicit_newline_at_end_gives_trailing_empty_row() {
        assert_eq!(wrap_input_display("hi\n", 20), vec!["hi", ""]);
    }

    #[test]
    fn wrap_combines_explicit_newlines_and_width_wrapping() {
        assert_eq!(wrap_input_display("abcde\nfg", 3), vec!["abc", "de", "fg"]);
    }

    #[test]
    fn wrap_width_zero_returns_lines_unsplit() {
        assert_eq!(
            wrap_input_display("a very long line", 0),
            vec!["a very long line"]
        );
    }

    #[test]
    fn wrap_width_zero_with_newlines_splits_on_newlines_only() {
        assert_eq!(wrap_input_display("foo\nbar", 0), vec!["foo", "bar"]);
    }

    #[test]
    fn wrap_wide_chars_counted_by_display_width() {
        let rows = wrap_input_display("\u{4e2d}\u{4e2d}\u{4e2d}", 4);
        assert_eq!(rows, vec!["\u{4e2d}\u{4e2d}", "\u{4e2d}"]);
    }

    // ── cursor_display_pos ──────────────────────────────────────────────────

    #[test]
    fn cursor_at_start_of_empty_input() {
        assert_eq!(cursor_display_pos("", 0, 10), (0, 0));
    }

    #[test]
    fn cursor_at_end_of_single_line() {
        assert_eq!(cursor_display_pos("hello", 5, 10), (0, 5));
    }

    #[test]
    fn cursor_mid_single_line() {
        assert_eq!(cursor_display_pos("hello", 2, 10), (0, 2));
    }

    #[test]
    fn cursor_wraps_to_second_row() {
        assert_eq!(cursor_display_pos("abcdef", 5, 5), (1, 0));
    }

    #[test]
    fn cursor_after_explicit_newline() {
        assert_eq!(cursor_display_pos("hi\n", 3, 20), (1, 0));
    }

    #[test]
    fn cursor_on_second_explicit_line() {
        assert_eq!(cursor_display_pos("foo\nbar", 7, 10), (1, 3));
    }

    #[test]
    fn cursor_explicit_newline_combined_with_wrap() {
        let s = "abc\ndefgh";
        assert_eq!(cursor_display_pos(s, s.len(), 3), (2, 2));
    }

    #[test]
    fn cursor_width_zero_no_wrapping() {
        assert_eq!(cursor_display_pos("hello world", 5, 0), (0, 5));
    }

    #[test]
    fn cursor_wide_char_advances_by_display_width() {
        let s = "\u{4e2d}\u{4e2d}";
        assert_eq!(cursor_display_pos(s, 3, 4), (0, 2));
        assert_eq!(cursor_display_pos(s, 6, 4), (0, 4));
    }

    #[test]
    fn cursor_wide_char_at_boundary_with_more_content() {
        let s = "\u{4e2d}\u{4e2d}\u{4e2d}";
        assert_eq!(cursor_display_pos(s, 6, 4), (1, 0));
    }

    #[test]
    fn cursor_at_exact_line_boundary_no_more_content() {
        assert_eq!(cursor_display_pos("abcde", 5, 5), (0, 5));
    }

    // ── byte_offset_at_display_pos tests ─────────────────────────────────────

    #[test]
    fn byte_offset_single_row_exact_col() {
        // "hello", width=80: row 0, col 3 → byte 3
        assert_eq!(byte_offset_at_display_pos("hello", 0, 3, 80), 3);
    }

    #[test]
    fn byte_offset_col_past_end_of_row_clamps_to_end() {
        // "hi\nworld", row 0 only has 2 chars; col 10 → byte 2 (before '\n')
        assert_eq!(byte_offset_at_display_pos("hi\nworld", 0, 10, 80), 2);
    }

    #[test]
    fn byte_offset_second_logical_row() {
        // "hello\nworld", row 1 col 3 → byte 9 ('l' in "world")
        assert_eq!(byte_offset_at_display_pos("hello\nworld", 1, 3, 80), 9);
    }

    #[test]
    fn byte_offset_soft_wrapped_row() {
        // "abcdef" with width=3 wraps: row0="abc", row1="def"
        // row 1, col 1 → byte 4 ('e')
        assert_eq!(byte_offset_at_display_pos("abcdef", 1, 1, 3), 4);
    }

    #[test]
    fn byte_offset_col_past_soft_wrapped_row_end() {
        // "abcdef" width=3: row 0 ends at byte 3; col 10 clamps to byte 3
        assert_eq!(byte_offset_at_display_pos("abcdef", 0, 10, 3), 3);
    }

    #[test]
    fn byte_offset_end_of_input() {
        // row beyond content → input.len()
        assert_eq!(byte_offset_at_display_pos("hello", 5, 0, 80), 5);
    }

    // ── find_at_context ───────────────────────────────────────────────────────

    #[test]
    fn find_at_context_no_at_returns_none() {
        assert!(find_at_context("hello world", 11).is_none());
    }

    #[test]
    fn find_at_context_bare_at_returns_at_with_empty_query() {
        let input = "say @";
        let result = find_at_context(input, input.len());
        assert_eq!(result, Some((4, "")));
    }

    #[test]
    fn find_at_context_partial_username() {
        let input = "hello @ali";
        let result = find_at_context(input, input.len());
        assert_eq!(result, Some((6, "ali")));
    }

    #[test]
    fn find_at_context_space_after_at_returns_none() {
        let input = "@ alice";
        assert!(find_at_context(input, input.len()).is_none());
    }

    #[test]
    fn find_at_context_newline_in_query_returns_none() {
        let input = "@ali\nce";
        assert!(find_at_context(input, input.len()).is_none());
    }

    #[test]
    fn find_at_context_cursor_before_at_returns_none() {
        let input = "hello @ali";
        assert!(find_at_context(input, 4).is_none());
    }

    #[test]
    fn find_at_context_uses_last_at() {
        let input = "@first @sec";
        let result = find_at_context(input, input.len());
        assert_eq!(result, Some((7, "sec")));
    }

    // ── prev_word_start ───────────────────────────────────────────────────────

    #[test]
    fn prev_word_start_from_end_of_word() {
        // "hello world", cursor at end → previous word start = 6 ("world")
        assert_eq!(prev_word_start("hello world", 11), 6);
    }

    #[test]
    fn prev_word_start_from_mid_word() {
        // "hello world", cursor at 8 (mid "world") → word start = 6
        assert_eq!(prev_word_start("hello world", 8), 6);
    }

    #[test]
    fn prev_word_start_skips_trailing_whitespace() {
        // "hello world  ", cursor at 13 (after trailing spaces) → "world" starts at 6
        assert_eq!(prev_word_start("hello world  ", 13), 6);
    }

    #[test]
    fn prev_word_start_at_beginning_returns_zero() {
        assert_eq!(prev_word_start("hello", 0), 0);
    }

    #[test]
    fn prev_word_start_from_first_word_returns_zero() {
        // "hello world", cursor in "hello" → 0
        assert_eq!(prev_word_start("hello world", 3), 0);
    }

    #[test]
    fn prev_word_start_single_word_no_spaces() {
        assert_eq!(prev_word_start("hello", 5), 0);
    }

    #[test]
    fn prev_word_start_multiple_spaces_between_words() {
        // "foo   bar", cursor at 9 → "bar" starts at 6
        assert_eq!(prev_word_start("foo   bar", 9), 6);
    }

    #[test]
    fn prev_word_start_unicode_word() {
        // "α β", cursor at 5 (after β which is 2 bytes) → 3 (start of β)
        let s = "α β"; // α=2 bytes, space=1, β=2 bytes → len=5
        assert_eq!(prev_word_start(s, 5), 3);
    }

    // ── next_word_end ─────────────────────────────────────────────────────────

    #[test]
    fn next_word_end_from_start() {
        // "hello world", cursor at 0 → end of "hello" = 5
        assert_eq!(next_word_end("hello world", 0), 5);
    }

    #[test]
    fn next_word_end_from_mid_word() {
        // "hello world", cursor at 2 → end of "hello" = 5
        assert_eq!(next_word_end("hello world", 2), 5);
    }

    #[test]
    fn next_word_end_skips_leading_whitespace() {
        // "hello world", cursor at 5 (space) → end of "world" = 11
        assert_eq!(next_word_end("hello world", 5), 11);
    }

    #[test]
    fn next_word_end_multiple_spaces() {
        // "foo   bar", cursor at 3 → end of "bar" = 9
        assert_eq!(next_word_end("foo   bar", 3), 9);
    }

    #[test]
    fn next_word_end_at_end_returns_len() {
        assert_eq!(next_word_end("hello", 5), 5);
    }

    #[test]
    fn next_word_end_only_spaces_returns_len() {
        assert_eq!(next_word_end("   ", 0), 3);
    }

    #[test]
    fn next_word_end_unicode_word() {
        // "α β", cursor at 0 → end of "α" = 2
        let s = "α β";
        assert_eq!(next_word_end(s, 0), 2);
    }
}
