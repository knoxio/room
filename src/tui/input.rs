use unicode_width::UnicodeWidthChar;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::widgets::{CommandPalette, MentionPicker, PALETTE_COMMANDS};

/// All mutable TUI input state. Pure data — no async context or I/O.
pub(super) struct InputState {
    pub(super) input: String,
    pub(super) cursor_pos: usize,
    pub(super) input_row_scroll: usize,
    pub(super) scroll_offset: usize,
    pub(super) palette: CommandPalette,
    pub(super) mention: MentionPicker,
}

impl InputState {
    pub(super) fn new() -> Self {
        Self {
            input: String::new(),
            cursor_pos: 0,
            input_row_scroll: 0,
            scroll_offset: 0,
            palette: CommandPalette::new(PALETTE_COMMANDS),
            mention: MentionPicker::new(),
        }
    }
}

/// The outcome of a key event that requires action from the caller.
pub(super) enum Action {
    /// The user confirmed input — send this payload string to the broker.
    Send(String),
    /// The user pressed Ctrl-C — exit the TUI.
    Quit,
}

/// Handle a single key event, mutating `state` in place.
///
/// Returns `Some(Action::Send(payload))` when the user submits a message,
/// `Some(Action::Quit)` on Ctrl-C, and `None` for all other events (cursor
/// moves, character insertion, popup navigation, etc.).
///
/// This function has no async context and no broker connection — every
/// key binding is unit-testable without spinning up a terminal.
pub(super) fn handle_key(
    key: KeyEvent,
    state: &mut InputState,
    online_users: &[String],
    visible_count: usize,
    input_width: usize,
) -> Option<Action> {
    match key.code {
        KeyCode::Esc => {
            if state.mention.active {
                state.mention.deactivate();
            } else if state.palette.active {
                state.palette.deactivate();
            }
        }
        KeyCode::Tab if state.mention.active => {
            if let Some(user) = state.mention.selected_user() {
                let user = user.to_owned();
                let replacement = format!("@{user} ");
                let query_end = state.cursor_pos;
                let at_start = state.mention.at_byte;
                state.input.replace_range(at_start..query_end, &replacement);
                state.cursor_pos = at_start + replacement.len();
                state.input_row_scroll = 0;
            }
            state.mention.deactivate();
        }
        KeyCode::Tab if state.palette.active => {
            if let Some(usage) = state.palette.selected_usage() {
                state.input = usage.to_owned();
                state.cursor_pos = state.input.len();
                state.input_row_scroll = 0;
            }
            state.palette.deactivate();
        }
        KeyCode::Enter => {
            if state.mention.active {
                if let Some(user) = state.mention.selected_user() {
                    let user = user.to_owned();
                    let replacement = format!("@{user} ");
                    let query_end = state.cursor_pos;
                    let at_start = state.mention.at_byte;
                    state.input.replace_range(at_start..query_end, &replacement);
                    state.cursor_pos = at_start + replacement.len();
                    state.input_row_scroll = 0;
                }
                state.mention.deactivate();
            } else if state.palette.active {
                if let Some(usage) = state.palette.selected_usage() {
                    state.input = usage.to_owned();
                    state.cursor_pos = state.input.len();
                    state.input_row_scroll = 0;
                }
                state.palette.deactivate();
            } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                // Shift+Enter: insert a newline at the cursor.
                state.input.insert(state.cursor_pos, '\n');
                state.cursor_pos += 1;
            } else if let Some(new_pos) = apply_backslash_enter(&mut state.input, state.cursor_pos)
            {
                // Backslash + Enter: strip the trailing '\' and insert a newline.
                state.cursor_pos = new_pos;
            } else if !state.input.is_empty() {
                let payload = build_payload(&state.input);
                state.input.clear();
                state.cursor_pos = 0;
                state.input_row_scroll = 0;
                state.scroll_offset = 0;
                return Some(Action::Send(payload));
            }
        }
        KeyCode::Up => {
            if state.mention.active {
                state.mention.move_up();
            } else if state.palette.active {
                state.palette.move_up();
            } else {
                let (cur_row, cur_col) =
                    cursor_display_pos(&state.input, state.cursor_pos, input_width);
                if cur_row > 0 {
                    state.cursor_pos =
                        byte_offset_at_display_pos(&state.input, cur_row - 1, cur_col, input_width);
                } else {
                    state.scroll_offset = state.scroll_offset.saturating_add(1);
                }
            }
        }
        KeyCode::Down => {
            if state.mention.active {
                state.mention.move_down();
            } else if state.palette.active {
                state.palette.move_down();
            } else {
                let (cur_row, cur_col) =
                    cursor_display_pos(&state.input, state.cursor_pos, input_width);
                let last_row = cursor_display_pos(&state.input, state.input.len(), input_width).0;
                if cur_row < last_row {
                    state.cursor_pos =
                        byte_offset_at_display_pos(&state.input, cur_row + 1, cur_col, input_width);
                } else {
                    state.scroll_offset = state.scroll_offset.saturating_sub(1);
                }
            }
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Some(Action::Quit);
        }
        KeyCode::Char(c) => {
            state.input.insert(state.cursor_pos, c);
            state.cursor_pos += c.len_utf8();
            // Update mention picker state.
            if let Some((at_byte, query)) = find_at_context(&state.input, state.cursor_pos) {
                if state.mention.active || c == '@' {
                    state.mention.activate(at_byte, online_users, query);
                    if state.mention.filtered.is_empty() {
                        state.mention.deactivate();
                    }
                }
            } else {
                state.mention.deactivate();
            }
            // Activate slash palette when `/` is typed into an empty input.
            if c == '/' && state.input == "/" {
                state.palette.activate();
            } else if state.palette.active {
                if let Some(query) = state.input.strip_prefix('/') {
                    state.palette.update_filter(query);
                    if state.palette.filtered.is_empty() {
                        state.palette.deactivate();
                    }
                } else {
                    state.palette.deactivate();
                }
            }
        }
        KeyCode::Backspace => {
            if state.cursor_pos > 0 {
                let prev = state.input[..state.cursor_pos]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                state.input.remove(prev);
                state.cursor_pos = prev;
                // Sync mention picker after deletion.
                if state.mention.active {
                    if let Some((at_byte, query)) = find_at_context(&state.input, state.cursor_pos)
                    {
                        state.mention.at_byte = at_byte;
                        state.mention.update_filter(online_users, query);
                        if state.mention.filtered.is_empty() {
                            state.mention.deactivate();
                        }
                    } else {
                        state.mention.deactivate();
                    }
                }
                // Sync palette state after deletion.
                if state.palette.active {
                    if state.input.is_empty() {
                        state.palette.deactivate();
                    } else if let Some(query) = state.input.strip_prefix('/') {
                        state.palette.update_filter(query);
                        if state.palette.filtered.is_empty() {
                            state.palette.deactivate();
                        }
                    } else {
                        state.palette.deactivate();
                    }
                }
            }
        }
        KeyCode::Left => {
            if state.cursor_pos > 0 {
                state.cursor_pos = state.input[..state.cursor_pos]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
            }
        }
        KeyCode::Right => {
            if state.cursor_pos < state.input.len() {
                let ch = state.input[state.cursor_pos..].chars().next().unwrap();
                state.cursor_pos += ch.len_utf8();
            }
        }
        KeyCode::Home => {
            state.cursor_pos = 0;
        }
        KeyCode::End => {
            state.cursor_pos = state.input.len();
        }
        KeyCode::PageUp => {
            state.scroll_offset = state.scroll_offset.saturating_add(visible_count);
        }
        KeyCode::PageDown => {
            state.scroll_offset = state.scroll_offset.saturating_sub(visible_count);
        }
        _ => {}
    }
    None
}

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

/// If the char immediately before `cursor_pos` in `buf` is `\`, removes it
/// and inserts `\n`, returning the new cursor position. Returns `None` if the
/// precondition is not met (no preceding backslash or cursor at start).
///
/// This mirrors the backslash+Enter key binding in the TUI event loop.
pub(super) fn apply_backslash_enter(buf: &mut String, cursor_pos: usize) -> Option<usize> {
    if cursor_pos > 0 && buf[..cursor_pos].ends_with('\\') {
        let bs_pos = cursor_pos - 1; // '\\' is ASCII (1 byte)
        buf.remove(bs_pos);
        buf.insert(bs_pos, '\n');
        Some(bs_pos + 1)
    } else {
        None
    }
}

/// Convert TUI input to a JSON envelope for the broker.
pub(super) fn build_payload(input: &str) -> String {
    // `/dm <user> <message>` — preserve spaces in the message body.
    if let Some(rest) = input.strip_prefix("/dm ") {
        let mut parts = rest.splitn(2, ' ');
        let to = parts.next().unwrap_or("").to_owned();
        let content = parts.next().unwrap_or("").to_owned();
        return serde_json::json!({ "type": "dm", "to": to, "content": content }).to_string();
    }
    if let Some(rest) = input.strip_prefix('/') {
        let mut parts = rest.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_owned();
        let params: Vec<String> = parts
            .next()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        serde_json::json!({ "type": "command", "cmd": cmd, "params": params }).to_string()
    } else {
        serde_json::json!({ "type": "message", "content": input }).to_string()
    }
}

/// Seed `online_users` from the broker's `/who` response content.
///
/// The broker sends `"online — alice, bob: away, charlie"` (or `"no users online"`).
/// Each entry is either a bare username or `username: status`; we extract the username part.
/// Merges into the existing list without removing users added by Join events.
pub(super) fn seed_online_users_from_who(content: &str, online_users: &mut Vec<String>) {
    if let Some(rest) = content.strip_prefix("online \u{2014} ") {
        for entry in rest.split(", ") {
            let username = entry.split(':').next().unwrap_or(entry).trim().to_owned();
            if !username.is_empty() && !online_users.contains(&username) {
                online_users.push(username);
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── seed_online_users_from_who tests ─────────────────────────────────────

    #[test]
    fn seed_who_populates_users() {
        let mut users = Vec::new();
        seed_online_users_from_who("online \u{2014} alice, bob, charlie", &mut users);
        assert_eq!(users, ["alice", "bob", "charlie"]);
    }

    #[test]
    fn seed_who_strips_status_suffix() {
        let mut users = Vec::new();
        seed_online_users_from_who("online \u{2014} alice: away, bob: coding", &mut users);
        assert_eq!(users, ["alice", "bob"]);
    }

    #[test]
    fn seed_who_no_users_online_is_noop() {
        let mut users = Vec::new();
        seed_online_users_from_who("no users online", &mut users);
        assert!(users.is_empty());
    }

    #[test]
    fn seed_who_does_not_duplicate_existing_users() {
        let mut users = vec!["alice".to_owned()];
        seed_online_users_from_who("online \u{2014} alice, bob", &mut users);
        assert_eq!(users, ["alice", "bob"]);
    }

    #[test]
    fn seed_who_unrelated_system_message_is_noop() {
        let mut users = Vec::new();
        seed_online_users_from_who("alice set status: away", &mut users);
        assert!(users.is_empty());
    }

    // ── build_payload tests ───────────────────────────────────────────────────

    #[test]
    fn build_payload_plain_text_is_message_type() {
        let payload = build_payload("hello world");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"], "hello world");
    }

    #[test]
    fn build_payload_dm_command() {
        let payload = build_payload("/dm alice hey there");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "dm");
        assert_eq!(v["to"], "alice");
        assert_eq!(v["content"], "hey there");
    }

    #[test]
    fn build_payload_slash_command_becomes_command_type() {
        let payload = build_payload("/claim issue #42");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "claim");
    }

    #[test]
    fn build_payload_who_command() {
        let payload = build_payload("/who");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "command");
        assert_eq!(v["cmd"], "who");
    }

    #[test]
    fn build_payload_dm_preserves_spaces_in_content() {
        let payload = build_payload("/dm bob hello   world");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["content"], "hello   world");
    }

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

    // ── apply_backslash_enter ───────────────────────────────────────────────

    #[test]
    fn backslash_enter_at_end_replaces_backslash_with_newline() {
        let mut buf = String::from("hello\\");
        let pos = apply_backslash_enter(&mut buf, 6);
        assert_eq!(pos, Some(6));
        assert_eq!(buf, "hello\n");
    }

    #[test]
    fn backslash_enter_mid_buffer_replaces_at_cursor() {
        let mut buf = String::from("foo\\bar");
        let pos = apply_backslash_enter(&mut buf, 4);
        assert_eq!(pos, Some(4));
        assert_eq!(buf, "foo\nbar");
    }

    #[test]
    fn backslash_enter_no_preceding_backslash_returns_none() {
        let mut buf = String::from("hello");
        let pos = apply_backslash_enter(&mut buf, 5);
        assert_eq!(pos, None);
        assert_eq!(buf, "hello");
    }

    #[test]
    fn backslash_enter_cursor_at_start_returns_none() {
        let mut buf = String::from("\\hello");
        let pos = apply_backslash_enter(&mut buf, 0);
        assert_eq!(pos, None);
        assert_eq!(buf, "\\hello");
    }

    #[test]
    fn backslash_enter_empty_buffer_returns_none() {
        let mut buf = String::new();
        let pos = apply_backslash_enter(&mut buf, 0);
        assert_eq!(pos, None);
    }

    #[test]
    fn backslash_enter_cursor_not_at_backslash_returns_none() {
        let mut buf = String::from("a\\b");
        let pos = apply_backslash_enter(&mut buf, 1);
        assert_eq!(pos, None);
        assert_eq!(buf, "a\\b");
    }

    #[test]
    fn backslash_enter_double_backslash_replaces_last_one() {
        let mut buf = String::from("foo\\\\");
        let pos = apply_backslash_enter(&mut buf, 5);
        assert_eq!(pos, Some(5));
        assert_eq!(buf, "foo\\\n");
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

    // ── handle_key unit tests ─────────────────────────────────────────────────

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn typing_chars_appends_to_input() {
        let mut state = InputState::new();
        handle_key(make_key(KeyCode::Char('h')), &mut state, &[], 10, 80);
        handle_key(make_key(KeyCode::Char('i')), &mut state, &[], 10, 80);
        assert_eq!(state.input, "hi");
        assert_eq!(state.cursor_pos, 2);
    }

    #[test]
    fn enter_on_empty_input_does_nothing() {
        let mut state = InputState::new();
        let action = handle_key(make_key(KeyCode::Enter), &mut state, &[], 10, 80);
        assert!(action.is_none());
        assert!(state.input.is_empty());
    }

    #[test]
    fn enter_on_nonempty_input_returns_send() {
        let mut state = InputState::new();
        state.input = "hello".to_owned();
        state.cursor_pos = 5;
        let action = handle_key(make_key(KeyCode::Enter), &mut state, &[], 10, 80);
        assert!(matches!(action, Some(Action::Send(_))));
        assert!(state.input.is_empty());
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn ctrl_c_returns_quit() {
        let mut state = InputState::new();
        let action = handle_key(
            make_key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state,
            &[],
            10,
            80,
        );
        assert!(matches!(action, Some(Action::Quit)));
    }

    #[test]
    fn backspace_removes_last_char() {
        let mut state = InputState::new();
        state.input = "hi".to_owned();
        state.cursor_pos = 2;
        handle_key(make_key(KeyCode::Backspace), &mut state, &[], 10, 80);
        assert_eq!(state.input, "h");
        assert_eq!(state.cursor_pos, 1);
    }

    #[test]
    fn left_arrow_moves_cursor_back() {
        let mut state = InputState::new();
        state.input = "abc".to_owned();
        state.cursor_pos = 3;
        handle_key(make_key(KeyCode::Left), &mut state, &[], 10, 80);
        assert_eq!(state.cursor_pos, 2);
    }

    #[test]
    fn right_arrow_moves_cursor_forward() {
        let mut state = InputState::new();
        state.input = "abc".to_owned();
        state.cursor_pos = 1;
        handle_key(make_key(KeyCode::Right), &mut state, &[], 10, 80);
        assert_eq!(state.cursor_pos, 2);
    }

    #[test]
    fn home_moves_cursor_to_start() {
        let mut state = InputState::new();
        state.input = "hello".to_owned();
        state.cursor_pos = 5;
        handle_key(make_key(KeyCode::Home), &mut state, &[], 10, 80);
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn end_moves_cursor_to_end() {
        let mut state = InputState::new();
        state.input = "hello".to_owned();
        state.cursor_pos = 0;
        handle_key(make_key(KeyCode::End), &mut state, &[], 10, 80);
        assert_eq!(state.cursor_pos, 5);
    }

    /// Up on single-line (row 0) input scrolls message history.
    #[test]
    fn up_arrow_on_single_line_scrolls_history() {
        let mut state = InputState::new();
        state.scroll_offset = 0;
        handle_key(make_key(KeyCode::Up), &mut state, &[], 10, 80);
        assert_eq!(state.scroll_offset, 1);
    }

    /// Down on single-line input (already at last row) decrements scroll.
    #[test]
    fn down_arrow_on_single_line_clamps_scroll_at_zero() {
        let mut state = InputState::new();
        state.scroll_offset = 0;
        handle_key(make_key(KeyCode::Down), &mut state, &[], 10, 80);
        assert_eq!(state.scroll_offset, 0);
    }

    /// Up on second row of multiline input moves cursor to first row.
    #[test]
    fn up_arrow_on_multiline_moves_cursor_to_previous_row() {
        let mut state = InputState::new();
        state.input = "hello\nworld".to_owned();
        // cursor at 'o' (index 8) on row 1, col 2
        state.cursor_pos = 8;
        state.scroll_offset = 5;
        handle_key(make_key(KeyCode::Up), &mut state, &[], 10, 80);
        // Should move to row 0 col 2 = byte 2 ('l' in "hello")
        assert_eq!(state.cursor_pos, 2);
        // scroll_offset must not change
        assert_eq!(state.scroll_offset, 5);
    }

    /// Down on first row of multiline input moves cursor to second row.
    #[test]
    fn down_arrow_on_multiline_moves_cursor_to_next_row() {
        let mut state = InputState::new();
        state.input = "hello\nworld".to_owned();
        // cursor at 'l' (index 2) on row 0, col 2
        state.cursor_pos = 2;
        state.scroll_offset = 3;
        handle_key(make_key(KeyCode::Down), &mut state, &[], 10, 80);
        // Should move to row 1 col 2 = byte 8 ('r' in "world")
        assert_eq!(state.cursor_pos, 8);
        assert_eq!(state.scroll_offset, 3);
    }

    /// Up on the first row of a multiline input scrolls history, not moves cursor.
    #[test]
    fn up_arrow_on_first_row_of_multiline_scrolls_history() {
        let mut state = InputState::new();
        state.input = "hello\nworld".to_owned();
        state.cursor_pos = 2; // row 0
        state.scroll_offset = 0;
        handle_key(make_key(KeyCode::Up), &mut state, &[], 10, 80);
        assert_eq!(state.scroll_offset, 1);
        assert_eq!(state.cursor_pos, 2); // unchanged
    }

    /// Down on the last row of a multiline input scrolls history.
    #[test]
    fn down_arrow_on_last_row_of_multiline_scrolls_history() {
        let mut state = InputState::new();
        state.input = "hello\nworld".to_owned();
        state.cursor_pos = 8; // row 1 (last)
        state.scroll_offset = 4;
        handle_key(make_key(KeyCode::Down), &mut state, &[], 10, 80);
        assert_eq!(state.scroll_offset, 3); // decremented
        assert_eq!(state.cursor_pos, 8); // unchanged
    }

    /// Up clamps target column to the end of a shorter previous row.
    #[test]
    fn up_arrow_clamps_column_to_end_of_shorter_row() {
        let mut state = InputState::new();
        // row 0 = "hi" (2 chars), row 1 = "world" (5 chars)
        state.input = "hi\nworld".to_owned();
        // cursor at 'l' (index 6) on row 1, col 3
        state.cursor_pos = 6;
        handle_key(make_key(KeyCode::Up), &mut state, &[], 10, 80);
        // row 0 only has col 0-2; col 3 clamps to end = byte 2 (position of '\n')
        assert_eq!(state.cursor_pos, 2);
    }

    #[test]
    fn page_up_scrolls_by_visible_count() {
        let mut state = InputState::new();
        handle_key(make_key(KeyCode::PageUp), &mut state, &[], 10, 80);
        assert_eq!(state.scroll_offset, 10);
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut state = InputState::new();
        state.input = "hello".to_owned();
        state.cursor_pos = 5;
        handle_key(
            make_key_mod(KeyCode::Enter, KeyModifiers::SHIFT),
            &mut state,
            &[],
            10,
            80,
        );
        assert_eq!(state.input, "hello\n");
        assert_eq!(state.cursor_pos, 6);
    }

    #[test]
    fn slash_activates_palette() {
        let mut state = InputState::new();
        handle_key(make_key(KeyCode::Char('/')), &mut state, &[], 10, 80);
        assert!(state.palette.active);
    }

    #[test]
    fn esc_deactivates_palette() {
        let mut state = InputState::new();
        state.palette.activate();
        handle_key(make_key(KeyCode::Esc), &mut state, &[], 10, 80);
        assert!(!state.palette.active);
    }

    #[test]
    fn at_char_activates_mention_picker_when_users_present() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned(), "bob".to_owned()];
        handle_key(make_key(KeyCode::Char('@')), &mut state, &users, 10, 80);
        assert!(state.mention.active);
    }

    #[test]
    fn send_payload_is_valid_json_message() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 11;
        let action = handle_key(make_key(KeyCode::Enter), &mut state, &[], 10, 80);
        if let Some(Action::Send(payload)) = action {
            let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(v["type"], "message");
            assert_eq!(v["content"], "hello world");
        } else {
            panic!("expected Action::Send");
        }
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
}
