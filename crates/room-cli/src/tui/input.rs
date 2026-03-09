use std::collections::HashMap;

use unicode_width::UnicodeWidthChar;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::widgets::{CommandPalette, MentionPicker};

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
            palette: CommandPalette::from_commands(crate::plugin::all_known_commands()),
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
    /// Switch to the next tab (Ctrl+N).
    NextTab,
    /// Switch to the previous tab (Ctrl+P).
    PrevTab,
    /// Switch to a specific tab by index (Ctrl+1–9).
    SwitchTab(usize),
    /// Open or reuse a DM room with the target user and send a message.
    DmRoom {
        target_user: String,
        content: String,
    },
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
        KeyCode::Esc => handle_esc(state),
        KeyCode::Tab if state.mention.active => handle_tab_mention(state),
        KeyCode::Tab if state.palette.active => {
            complete_palette_selection(state, online_users);
        }
        KeyCode::Enter => return handle_enter(key, state, online_users),
        KeyCode::Up => handle_up(state, online_users, input_width),
        KeyCode::Down => handle_down(state, online_users, input_width),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Some(Action::Quit);
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Some(Action::NextTab);
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Some(Action::PrevTab);
        }
        KeyCode::Char(c @ '1'..='9') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Some(Action::SwitchTab((c as usize) - ('1' as usize)));
        }
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
            state.cursor_pos = prev_word_start(&state.input, state.cursor_pos);
            sync_mention_after_cursor_move(state, online_users);
        }
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
            state.cursor_pos = next_word_end(&state.input, state.cursor_pos);
            sync_mention_after_cursor_move(state, online_users);
        }
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => {
            handle_word_delete(state, online_users);
        }
        KeyCode::Char(c) => handle_char(c, state, online_users),
        KeyCode::Backspace => handle_backspace(state, online_users),
        KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
            state.cursor_pos = prev_word_start(&state.input, state.cursor_pos);
            sync_mention_after_cursor_move(state, online_users);
        }
        KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
            state.cursor_pos = next_word_end(&state.input, state.cursor_pos);
            sync_mention_after_cursor_move(state, online_users);
        }
        KeyCode::Left => handle_left(state, online_users),
        KeyCode::Right => handle_right(state, online_users),
        KeyCode::Home => {
            state.cursor_pos = 0;
            sync_mention_after_cursor_move(state, online_users);
        }
        KeyCode::End => {
            state.cursor_pos = state.input.len();
            sync_mention_after_cursor_move(state, online_users);
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

/// Dismiss mention picker, command palette, or clear input on Esc.
fn handle_esc(state: &mut InputState) {
    if state.mention.active {
        state.mention.deactivate();
    } else if state.palette.active {
        state.palette.deactivate();
    } else if !state.input.is_empty() {
        state.input.clear();
        state.cursor_pos = 0;
        state.input_row_scroll = 0;
    }
}

/// Complete the currently selected mention on Tab.
fn handle_tab_mention(state: &mut InputState) {
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

/// Handle the Enter key: mention/palette completion, newline insertion, DM
/// intercept, or message submission.
fn handle_enter(key: KeyEvent, state: &mut InputState, online_users: &[String]) -> Option<Action> {
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
        complete_palette_selection(state, online_users);
    } else if key.modifiers.contains(KeyModifiers::SHIFT) {
        state.input.insert(state.cursor_pos, '\n');
        state.cursor_pos += 1;
    } else if let Some(new_pos) = apply_backslash_enter(&mut state.input, state.cursor_pos) {
        state.cursor_pos = new_pos;
    } else if !state.input.is_empty() {
        if let Some(dm) = parse_dm_input(&state.input) {
            state.input.clear();
            state.cursor_pos = 0;
            state.input_row_scroll = 0;
            state.scroll_offset = 0;
            return Some(dm);
        }
        let payload = build_payload(&state.input);
        state.input.clear();
        state.cursor_pos = 0;
        state.input_row_scroll = 0;
        state.scroll_offset = 0;
        return Some(Action::Send(payload));
    }
    None
}

/// Navigate mention/palette picker upward, move cursor up in multi-line input,
/// or scroll the chat history.
fn handle_up(state: &mut InputState, online_users: &[String], input_width: usize) {
    if state.mention.active {
        state.mention.move_up();
    } else if state.palette.active {
        state.palette.move_up();
    } else {
        let (cur_row, cur_col) = cursor_display_pos(&state.input, state.cursor_pos, input_width);
        if cur_row > 0 {
            state.cursor_pos =
                byte_offset_at_display_pos(&state.input, cur_row - 1, cur_col, input_width);
            sync_mention_after_cursor_move(state, online_users);
        } else {
            state.scroll_offset = state.scroll_offset.saturating_add(1);
        }
    }
}

/// Navigate mention/palette picker downward, move cursor down in multi-line
/// input, or scroll the chat history.
fn handle_down(state: &mut InputState, online_users: &[String], input_width: usize) {
    if state.mention.active {
        state.mention.move_down();
    } else if state.palette.active {
        state.palette.move_down();
    } else {
        let (cur_row, cur_col) = cursor_display_pos(&state.input, state.cursor_pos, input_width);
        let last_row = cursor_display_pos(&state.input, state.input.len(), input_width).0;
        if cur_row < last_row {
            state.cursor_pos =
                byte_offset_at_display_pos(&state.input, cur_row + 1, cur_col, input_width);
            sync_mention_after_cursor_move(state, online_users);
        } else {
            state.scroll_offset = state.scroll_offset.saturating_sub(1);
        }
    }
}

/// Delete the previous word (Alt+Backspace).
fn handle_word_delete(state: &mut InputState, online_users: &[String]) {
    let word_start = prev_word_start(&state.input, state.cursor_pos);
    if word_start < state.cursor_pos {
        state.input.drain(word_start..state.cursor_pos);
        state.cursor_pos = word_start;
        sync_mention_after_delete(state, online_users);
        sync_palette_after_delete(state);
    }
}

/// Insert a character and update mention picker / command palette state.
fn handle_char(c: char, state: &mut InputState, online_users: &[String]) {
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

/// Delete the character before the cursor.
fn handle_backspace(state: &mut InputState, online_users: &[String]) {
    if state.cursor_pos > 0 {
        let prev = state.input[..state.cursor_pos]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        state.input.remove(prev);
        state.cursor_pos = prev;
        sync_mention_after_delete(state, online_users);
        sync_palette_after_delete(state);
    }
}

/// Move cursor one character to the left.
fn handle_left(state: &mut InputState, online_users: &[String]) {
    if state.cursor_pos > 0 {
        state.cursor_pos = state.input[..state.cursor_pos]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }
    sync_mention_after_cursor_move(state, online_users);
}

/// Move cursor one character to the right.
fn handle_right(state: &mut InputState, online_users: &[String]) {
    if state.cursor_pos < state.input.len() {
        let ch = state.input[state.cursor_pos..].chars().next().unwrap();
        state.cursor_pos += ch.len_utf8();
    }
    sync_mention_after_cursor_move(state, online_users);
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

/// Re-evaluate the mention picker after the cursor moved.
///
/// If the picker is active, checks the `@` context at the new cursor position.
/// Updates the filter if the context is still valid, or deactivates the picker
/// if the cursor has moved away from the `@` trigger.
fn sync_mention_after_cursor_move(state: &mut InputState, online_users: &[String]) {
    if !state.mention.active {
        return;
    }
    if let Some((at_byte, query)) = find_at_context(&state.input, state.cursor_pos) {
        state.mention.at_byte = at_byte;
        state.mention.update_filter(online_users, query);
        if state.mention.filtered.is_empty() {
            state.mention.deactivate();
        }
    } else {
        state.mention.deactivate();
    }
}

/// Re-evaluate the mention picker after a deletion (Backspace / Alt+Backspace).
///
/// If the picker is active, updates the filter or deactivates if the `@` context
/// is gone. If the picker is inactive, reactivates it when the cursor is inside
/// a valid `@` context with at least one match — this handles the case where the
/// picker was auto-dismissed because all matches disappeared, and a subsequent
/// deletion restores a matching prefix.
fn sync_mention_after_delete(state: &mut InputState, online_users: &[String]) {
    if let Some((at_byte, query)) = find_at_context(&state.input, state.cursor_pos) {
        if state.mention.active {
            state.mention.at_byte = at_byte;
            state.mention.update_filter(online_users, query);
            if state.mention.filtered.is_empty() {
                state.mention.deactivate();
            }
        } else {
            state.mention.activate(at_byte, online_users, query);
            if state.mention.filtered.is_empty() {
                state.mention.deactivate();
            }
        }
    } else {
        state.mention.deactivate();
    }
}

/// Complete the currently selected palette command.
///
/// If the selected command's first parameter is `Username`, sets the input to
/// `/<command> ` and activates the mention picker so the user can tab-complete
/// a username without typing `@`. Otherwise fills in the full usage string as
/// before.
fn complete_palette_selection(state: &mut InputState, online_users: &[String]) {
    let selected_idx = state.palette.filtered.get(state.palette.selected).copied();
    if let Some(idx) = selected_idx {
        let cmd_name = state.palette.commands[idx].cmd.clone();
        let is_username = state.palette.is_username_param(&cmd_name, 0);
        let has_choices = !state.palette.completions_at(&cmd_name, 0).is_empty();

        if is_username {
            // Set input to "/<cmd> " and activate mention picker at the space.
            let prefix = format!("/{cmd_name} ");
            let at_byte = prefix.len();
            state.input = prefix;
            state.cursor_pos = at_byte;
            state.input_row_scroll = 0;
            state.palette.deactivate();
            state.mention.activate(at_byte, online_users, "");
            if state.mention.filtered.is_empty() {
                state.mention.deactivate();
            }
        } else if has_choices {
            // Set input to "/<cmd> " so the user can type/pick a choice.
            let prefix = format!("/{cmd_name} ");
            state.input = prefix;
            state.cursor_pos = state.input.len();
            state.input_row_scroll = 0;
            state.palette.deactivate();
        } else if let Some(usage) = state.palette.selected_usage() {
            state.input = usage.to_owned();
            state.cursor_pos = state.input.len();
            state.input_row_scroll = 0;
            state.palette.deactivate();
        } else {
            state.palette.deactivate();
        }
    } else {
        state.palette.deactivate();
    }
}

/// Re-evaluate the command palette after a deletion (Backspace / Alt+Backspace).
///
/// If the palette is active, updates the filter or deactivates. If inactive,
/// reactivates when input starts with `/` and the query matches at least one
/// command — this handles the case where the palette was auto-dismissed and a
/// backspace restores a matching query.
fn sync_palette_after_delete(state: &mut InputState) {
    if state.input.is_empty() {
        state.palette.deactivate();
    } else if let Some(query) = state.input.strip_prefix('/') {
        state.palette.update_filter(query);
        if state.palette.filtered.is_empty() {
            state.palette.deactivate();
        } else if !state.palette.active {
            state.palette.active = true;
            state.palette.selected = 0;
        }
    } else {
        state.palette.deactivate();
    }
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

/// Parse `/dm <user> <message>` input into a `DmRoom` action.
///
/// Returns `Some(Action::DmRoom { .. })` when the input is a valid `/dm`
/// command with both a target user and message content. Returns `None` for
/// incomplete input (missing user or message) — those fall through to
/// `build_payload` for backwards-compatible intra-room DM handling.
fn parse_dm_input(input: &str) -> Option<Action> {
    let rest = input.strip_prefix("/dm ")?;
    let mut parts = rest.splitn(2, ' ');
    let target_user = parts.next().filter(|s| !s.is_empty())?;
    let content = parts.next().filter(|s| !s.is_empty())?;
    Some(Action::DmRoom {
        target_user: target_user.to_owned(),
        content: content.to_owned(),
    })
}

/// Convert TUI input to a JSON envelope for the broker.
pub(super) fn build_payload(input: &str) -> String {
    // `/dm <user> <message>` — preserve spaces in the message body.
    // NOTE: This branch is now only reached when `parse_dm_input` returns
    // None (e.g. `/dm user` with no message body).
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

/// Seed `online_users` and `user_statuses` from the broker's `/who` response content.
///
/// The broker sends `"online — alice, bob: away, charlie"` (or `"no users online"`).
/// Each entry is either a bare username or `username: status`.
/// Merges into the existing list without removing users added by Join events.
pub(super) fn seed_online_users_from_who(
    content: &str,
    online_users: &mut Vec<String>,
    user_statuses: &mut HashMap<String, String>,
) {
    if let Some(rest) = content.strip_prefix("online \u{2014} ") {
        for entry in rest.split(", ") {
            let (username, status) = match entry.split_once(": ") {
                Some((u, s)) => (u.trim().to_owned(), s.trim().to_owned()),
                None => (entry.trim().to_owned(), String::new()),
            };
            if !username.is_empty() {
                if !online_users.contains(&username) {
                    online_users.push(username.clone());
                }
                user_statuses.insert(username, status);
            }
        }
    }
}

/// Parse a `/set_status` system broadcast into `(username, status)`.
///
/// The broker broadcasts either:
/// - `"alice set status: busy"` → `Some(("alice", "busy"))`
/// - `"alice cleared their status"` → `Some(("alice", ""))`
pub(super) fn parse_status_broadcast(content: &str) -> Option<(String, String)> {
    if let Some(rest) = content.strip_suffix(" cleared their status") {
        return Some((rest.to_owned(), String::new()));
    }
    if let Some((name, status)) = content.split_once(" set status: ") {
        return Some((name.to_owned(), status.to_owned()));
    }
    None
}

/// Parse a `/kick` system broadcast into the kicked username.
///
/// The broker broadcasts `"alice kicked bob (token invalidated)"`.
/// Returns `Some("bob")` — the target user who was kicked.
pub(super) fn parse_kick_broadcast(content: &str) -> Option<&str> {
    let rest = content.strip_suffix(" (token invalidated)")?;
    let (_issuer, target) = rest.split_once(" kicked ")?;
    if target.is_empty() {
        return None;
    }
    Some(target)
}

/// Move the cursor to the start of the previous word.
///
/// "Word" is a maximal run of non-whitespace characters. Starting from
/// `cursor_pos`, the function first skips any trailing whitespace going
/// backwards (phase 1), then skips the preceding non-whitespace run (phase 2),
/// landing at the first byte of that word.
///
/// Returns 0 if there is no previous word.
fn prev_word_start(input: &str, cursor_pos: usize) -> usize {
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
fn next_word_end(input: &str, cursor_pos: usize) -> usize {
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

    // ── seed_online_users_from_who tests ─────────────────────────────────────

    #[test]
    fn seed_who_populates_users() {
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who(
            "online \u{2014} alice, bob, charlie",
            &mut users,
            &mut statuses,
        );
        assert_eq!(users, ["alice", "bob", "charlie"]);
        assert_eq!(statuses.get("alice").unwrap(), "");
    }

    #[test]
    fn seed_who_extracts_statuses() {
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who(
            "online \u{2014} alice: away, bob: coding, charlie",
            &mut users,
            &mut statuses,
        );
        assert_eq!(users, ["alice", "bob", "charlie"]);
        assert_eq!(statuses.get("alice").unwrap(), "away");
        assert_eq!(statuses.get("bob").unwrap(), "coding");
        assert_eq!(statuses.get("charlie").unwrap(), "");
    }

    #[test]
    fn seed_who_no_users_online_is_noop() {
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who("no users online", &mut users, &mut statuses);
        assert!(users.is_empty());
        assert!(statuses.is_empty());
    }

    #[test]
    fn seed_who_does_not_duplicate_existing_users() {
        let mut users = vec!["alice".to_owned()];
        let mut statuses = HashMap::new();
        seed_online_users_from_who("online \u{2014} alice, bob", &mut users, &mut statuses);
        assert_eq!(users, ["alice", "bob"]);
    }

    #[test]
    fn seed_who_unrelated_system_message_is_noop() {
        let mut users = Vec::new();
        let mut statuses = HashMap::new();
        seed_online_users_from_who("alice set status: away", &mut users, &mut statuses);
        assert!(users.is_empty());
        assert!(statuses.is_empty());
    }

    // ── parse_status_broadcast tests ─────────────────────────────────────────

    #[test]
    fn parse_status_set() {
        let result = parse_status_broadcast("alice set status: busy");
        assert_eq!(result, Some(("alice".to_owned(), "busy".to_owned())));
    }

    #[test]
    fn parse_status_cleared() {
        let result = parse_status_broadcast("alice cleared their status");
        assert_eq!(result, Some(("alice".to_owned(), String::new())));
    }

    #[test]
    fn parse_status_unrelated_message() {
        assert!(parse_status_broadcast("alice joined").is_none());
        assert!(parse_status_broadcast("hello world").is_none());
    }

    #[test]
    fn parse_status_with_spaces_in_name() {
        let result = parse_status_broadcast("my-agent set status: reviewing PR");
        assert_eq!(
            result,
            Some(("my-agent".to_owned(), "reviewing PR".to_owned()))
        );
    }

    // ── parse_kick_broadcast tests ──────────────────────────────────────────

    #[test]
    fn parse_kick_standard() {
        let result = parse_kick_broadcast("alice kicked bob (token invalidated)");
        assert_eq!(result, Some("bob"));
    }

    #[test]
    fn parse_kick_hyphenated_names() {
        let result = parse_kick_broadcast("room-host kicked my-agent (token invalidated)");
        assert_eq!(result, Some("my-agent"));
    }

    #[test]
    fn parse_kick_unrelated_message() {
        assert!(parse_kick_broadcast("alice set status: busy").is_none());
        assert!(parse_kick_broadcast("hello world").is_none());
        assert!(parse_kick_broadcast("alice cleared their status").is_none());
    }

    #[test]
    fn parse_kick_missing_suffix() {
        assert!(parse_kick_broadcast("alice kicked bob").is_none());
    }

    #[test]
    fn parse_kick_missing_target() {
        assert!(parse_kick_broadcast("alice kicked  (token invalidated)").is_none());
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

    // ── parse_dm_input tests ────────────────────────────────────────────────

    #[test]
    fn parse_dm_input_returns_dm_room_action() {
        let action = parse_dm_input("/dm alice hello there").unwrap();
        match action {
            Action::DmRoom {
                target_user,
                content,
            } => {
                assert_eq!(target_user, "alice");
                assert_eq!(content, "hello there");
            }
            _ => panic!("expected DmRoom action"),
        }
    }

    #[test]
    fn parse_dm_input_preserves_spaces_in_content() {
        let action = parse_dm_input("/dm bob hello   world").unwrap();
        match action {
            Action::DmRoom { content, .. } => {
                assert_eq!(content, "hello   world");
            }
            _ => panic!("expected DmRoom action"),
        }
    }

    #[test]
    fn parse_dm_input_returns_none_for_missing_content() {
        assert!(parse_dm_input("/dm alice").is_none());
    }

    #[test]
    fn parse_dm_input_returns_none_for_missing_user() {
        assert!(parse_dm_input("/dm ").is_none());
    }

    #[test]
    fn parse_dm_input_returns_none_for_non_dm() {
        assert!(parse_dm_input("/who").is_none());
        assert!(parse_dm_input("hello world").is_none());
    }

    #[test]
    fn parse_dm_input_returns_none_for_user_only_with_trailing_space() {
        // "/dm alice " has empty content after splitting
        assert!(parse_dm_input("/dm alice ").is_none());
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

    // ── Alt+word-skip via handle_key ──────────────────────────────────────────

    #[test]
    fn alt_left_moves_to_prev_word_start() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 11;
        handle_key(
            make_key_mod(KeyCode::Left, KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        assert_eq!(state.cursor_pos, 6);
    }

    #[test]
    fn alt_right_moves_to_next_word_end() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 0;
        handle_key(
            make_key_mod(KeyCode::Right, KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        assert_eq!(state.cursor_pos, 5);
    }

    #[test]
    fn alt_b_moves_to_prev_word_start() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 11;
        handle_key(
            make_key_mod(KeyCode::Char('b'), KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        assert_eq!(state.cursor_pos, 6);
    }

    #[test]
    fn alt_f_moves_to_next_word_end() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 0;
        handle_key(
            make_key_mod(KeyCode::Char('f'), KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        assert_eq!(state.cursor_pos, 5);
    }

    // ── Esc clears input (#157) ──────────────────────────────────────────────

    #[test]
    fn esc_clears_input_when_no_popup_active() {
        let mut state = InputState::new();
        state.input = "some text here".to_owned();
        state.cursor_pos = 14;
        handle_key(make_key(KeyCode::Esc), &mut state, &[], 10, 80);
        assert!(state.input.is_empty());
        assert_eq!(state.cursor_pos, 0);
        assert_eq!(state.input_row_scroll, 0);
    }

    #[test]
    fn esc_on_empty_input_is_noop() {
        let mut state = InputState::new();
        handle_key(make_key(KeyCode::Esc), &mut state, &[], 10, 80);
        assert!(state.input.is_empty());
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn esc_dismisses_palette_before_clearing_input() {
        let mut state = InputState::new();
        state.input = "/he".to_owned();
        state.cursor_pos = 3;
        state.palette.activate();
        handle_key(make_key(KeyCode::Esc), &mut state, &[], 10, 80);
        // First Esc dismisses palette, input stays
        assert!(!state.palette.active);
        assert_eq!(state.input, "/he");
        // Second Esc clears input
        handle_key(make_key(KeyCode::Esc), &mut state, &[], 10, 80);
        assert!(state.input.is_empty());
    }

    #[test]
    fn esc_dismisses_mention_before_clearing_input() {
        let mut state = InputState::new();
        state.input = "@al".to_owned();
        state.cursor_pos = 3;
        let users = vec!["alice".to_owned()];
        state.mention.activate(0, &users, "al");
        handle_key(make_key(KeyCode::Esc), &mut state, &users, 10, 80);
        // First Esc dismisses mention picker
        assert!(!state.mention.active);
        assert_eq!(state.input, "@al");
        // Second Esc clears input
        handle_key(make_key(KeyCode::Esc), &mut state, &users, 10, 80);
        assert!(state.input.is_empty());
    }

    // ── Alt+Backspace deletes word (#159) ────────────────────────────────────

    #[test]
    fn alt_backspace_deletes_last_word() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 11;
        handle_key(
            make_key_mod(KeyCode::Backspace, KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        assert_eq!(state.input, "hello ");
        assert_eq!(state.cursor_pos, 6);
    }

    #[test]
    fn alt_backspace_deletes_first_word() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 5;
        handle_key(
            make_key_mod(KeyCode::Backspace, KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        assert_eq!(state.input, " world");
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn alt_backspace_skips_trailing_spaces() {
        let mut state = InputState::new();
        state.input = "foo   bar".to_owned();
        state.cursor_pos = 6; // at 'b' — prev_word_start skips spaces then "foo"
        handle_key(
            make_key_mod(KeyCode::Backspace, KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        assert_eq!(state.input, "bar");
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn alt_backspace_at_start_is_noop() {
        let mut state = InputState::new();
        state.input = "hello".to_owned();
        state.cursor_pos = 0;
        handle_key(
            make_key_mod(KeyCode::Backspace, KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        assert_eq!(state.input, "hello");
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn alt_backspace_single_word_clears_all() {
        let mut state = InputState::new();
        state.input = "hello".to_owned();
        state.cursor_pos = 5;
        handle_key(
            make_key_mod(KeyCode::Backspace, KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        assert!(state.input.is_empty());
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn alt_backspace_mid_word_deletes_to_word_start() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 8; // at 'r' in "world" — prev_word_start = 6 ('w')
        handle_key(
            make_key_mod(KeyCode::Backspace, KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        // drains bytes 6..8 ("wo"), leaving "hello rld"
        assert_eq!(state.input, "hello rld");
        assert_eq!(state.cursor_pos, 6);
    }

    #[test]
    fn alt_backspace_unicode() {
        let mut state = InputState::new();
        // α=2bytes, ' '=1, β=2bytes, ' '=1, γ=2bytes → len=8
        state.input = "α β γ".to_owned();
        state.cursor_pos = state.input.len(); // 8
        handle_key(
            make_key_mod(KeyCode::Backspace, KeyModifiers::ALT),
            &mut state,
            &[],
            10,
            80,
        );
        // drains bytes 6..8 ("γ"), leaving "α β "
        assert_eq!(state.input, "α β ");
        assert_eq!(state.cursor_pos, 6);
    }

    // ── mention picker cursor sync (#151) ────────────────────────────────────

    #[test]
    fn mention_picker_deactivates_on_left_arrow_past_at() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        // Type "@" to activate mention picker.
        handle_key(make_key(KeyCode::Char('@')), &mut state, &users, 10, 80);
        assert!(state.mention.active);
        // Move cursor left — now before the '@', picker should deactivate.
        handle_key(make_key(KeyCode::Left), &mut state, &users, 10, 80);
        assert!(!state.mention.active);
    }

    #[test]
    fn mention_picker_deactivates_on_home() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        // Type "hi @" to activate mention picker.
        for c in "hi @".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(state.mention.active);
        // Home moves cursor to 0 — well before the '@'.
        handle_key(make_key(KeyCode::Home), &mut state, &users, 10, 80);
        assert!(!state.mention.active);
    }

    #[test]
    fn mention_picker_updates_filter_on_right_arrow() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned(), "bob".to_owned()];
        // Type "@ali" — picker active, filtered to alice.
        for c in "@ali".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(state.mention.active);
        assert_eq!(state.mention.filtered.len(), 1);
        // Move cursor left twice (to "@a|li" — cursor between 'a' and 'l').
        handle_key(make_key(KeyCode::Left), &mut state, &users, 10, 80);
        handle_key(make_key(KeyCode::Left), &mut state, &users, 10, 80);
        // Now filter query is "a" — both alice matches, bob doesn't.
        assert!(state.mention.active);
        assert_eq!(state.mention.filtered.len(), 1);
        assert_eq!(state.mention.filtered[0], "alice");
    }

    #[test]
    fn mention_picker_deactivates_on_end_with_space_after_at() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        // Type "@alice test" — picker deactivates when space is typed after @alice.
        for c in "@alice ".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(!state.mention.active);
        // Re-type "@b" to activate again.
        for c in "@b".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        // No user matches "b" so picker should be inactive.
        assert!(!state.mention.active);
    }

    #[test]
    fn mention_picker_stays_active_on_right_within_query() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        // Type "@al" — picker active.
        for c in "@al".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(state.mention.active);
        // Move left to "@a|l" then right back to "@al|".
        handle_key(make_key(KeyCode::Left), &mut state, &users, 10, 80);
        assert!(state.mention.active);
        handle_key(make_key(KeyCode::Right), &mut state, &users, 10, 80);
        assert!(state.mention.active);
    }

    #[test]
    fn mention_picker_deactivates_on_alt_left_past_at() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        // Type "hi @al" — picker active.
        for c in "hi @al".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(state.mention.active);
        // Alt+Left jumps to word start — past the '@'.
        handle_key(
            make_key_mod(KeyCode::Left, KeyModifiers::ALT),
            &mut state,
            &users,
            10,
            80,
        );
        // Cursor is now at the '@' position (byte 3). find_at_context with cursor
        // at '@' returns None (no query between '@' and cursor).
        assert!(!state.mention.active);
    }

    // ── Palette reactivation on backspace (#172) ─────────────────────────────

    #[test]
    fn palette_reactivates_on_backspace_after_auto_dismiss() {
        let mut state = InputState::new();
        // Type "/he" — palette activates and filters.
        for c in "/he".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &[], 10, 80);
        }
        assert!(state.palette.active);
        // Type "zzz" — no matches, palette auto-dismisses.
        for c in "zzz".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &[], 10, 80);
        }
        assert!(!state.palette.active);
        assert_eq!(state.input, "/hezzz");
        // Backspace 3 times to get back to "/he" — palette should reactivate.
        for _ in 0..3 {
            handle_key(make_key(KeyCode::Backspace), &mut state, &[], 10, 80);
        }
        assert_eq!(state.input, "/he");
        assert!(
            state.palette.active,
            "palette should reactivate when backspace restores a matching query"
        );
        assert!(
            !state.palette.filtered.is_empty(),
            "palette should have matches for 'he'"
        );
    }

    #[test]
    fn palette_stays_dismissed_when_backspace_query_has_no_matches() {
        let mut state = InputState::new();
        // Type "/zzz" — no matches, palette dismissed.
        for c in "/zzz".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &[], 10, 80);
        }
        assert!(!state.palette.active);
        // Backspace to "/zz" — still no matches, stays dismissed.
        handle_key(make_key(KeyCode::Backspace), &mut state, &[], 10, 80);
        assert!(!state.palette.active);
    }

    #[test]
    fn palette_deactivates_when_slash_is_deleted() {
        let mut state = InputState::new();
        // Type "/" — palette activates.
        handle_key(make_key(KeyCode::Char('/')), &mut state, &[], 10, 80);
        assert!(state.palette.active);
        // Backspace removes "/" — palette deactivates.
        handle_key(make_key(KeyCode::Backspace), &mut state, &[], 10, 80);
        assert!(state.input.is_empty());
        assert!(!state.palette.active);
    }

    // ── Mention picker reactivation on backspace (#172) ──────────────────────

    #[test]
    fn mention_picker_reactivates_on_backspace_after_auto_dismiss() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        // Type "@ali" — picker activates, "alice" matches.
        for c in "@ali".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(state.mention.active);
        // Type "zzz" — no matches, picker auto-dismisses.
        for c in "zzz".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(!state.mention.active);
        assert_eq!(state.input, "@alizzz");
        // Backspace 3 times to get back to "@ali" — picker should reactivate.
        for _ in 0..3 {
            handle_key(make_key(KeyCode::Backspace), &mut state, &users, 10, 80);
        }
        assert_eq!(state.input, "@ali");
        assert!(
            state.mention.active,
            "mention picker should reactivate when backspace restores a matching query"
        );
        assert_eq!(state.mention.filtered, vec!["alice".to_owned()]);
    }

    #[test]
    fn mention_picker_stays_dismissed_when_no_user_matches() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        // Type "@zzz" — no matches, picker dismissed.
        for c in "@zzz".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(!state.mention.active);
        // Backspace to "@zz" — still no user matches "zz".
        handle_key(make_key(KeyCode::Backspace), &mut state, &users, 10, 80);
        assert!(!state.mention.active);
    }

    // ── complete_palette_selection tests (#257) ───────────────────────────────

    #[test]
    fn tab_on_dm_activates_mention_picker() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned(), "bob".to_owned()];
        // Type "/dm" to activate and filter palette
        for c in "/dm".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(state.palette.active);
        // Tab to complete — should set input to "/dm " and activate mention picker
        handle_key(make_key(KeyCode::Tab), &mut state, &users, 10, 80);
        assert_eq!(state.input, "/dm ");
        assert!(!state.palette.active, "palette should deactivate");
        assert!(
            state.mention.active,
            "mention picker should activate for Username param"
        );
    }

    #[test]
    fn tab_on_kick_activates_mention_picker() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        for c in "/kick".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(state.palette.active);
        handle_key(make_key(KeyCode::Tab), &mut state, &users, 10, 80);
        assert_eq!(state.input, "/kick ");
        assert!(state.mention.active);
    }

    #[test]
    fn tab_on_stats_sets_short_prefix_for_choices() {
        let mut state = InputState::new();
        let users: Vec<String> = vec![];
        for c in "/stats".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        assert!(state.palette.active);
        handle_key(make_key(KeyCode::Tab), &mut state, &users, 10, 80);
        // stats has Choice param — should set "/stats " for the user to type a number
        assert_eq!(state.input, "/stats ");
        assert!(!state.palette.active);
        assert!(!state.mention.active);
    }

    #[test]
    fn tab_on_who_fills_full_usage() {
        let mut state = InputState::new();
        let users: Vec<String> = vec![];
        for c in "/who".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        handle_key(make_key(KeyCode::Tab), &mut state, &users, 10, 80);
        // who has no params — fills in the full usage string
        assert_eq!(state.input, "/who");
        assert!(!state.palette.active);
    }

    #[test]
    fn enter_on_dm_activates_mention_picker() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        for c in "/dm".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, 10, 80);
        }
        // Enter also completes palette selection
        handle_key(make_key(KeyCode::Enter), &mut state, &users, 10, 80);
        assert_eq!(state.input, "/dm ");
        assert!(state.mention.active);
    }

    // ── Tab switching keybinding tests ──────────────────────────────────────

    fn make_ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_n_returns_next_tab_action() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('n'), &mut state, &[], 10, 80);
        assert!(matches!(action, Some(Action::NextTab)));
    }

    #[test]
    fn ctrl_p_returns_prev_tab_action() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('p'), &mut state, &[], 10, 80);
        assert!(matches!(action, Some(Action::PrevTab)));
    }

    #[test]
    fn ctrl_1_returns_switch_tab_0() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('1'), &mut state, &[], 10, 80);
        assert!(matches!(action, Some(Action::SwitchTab(0))));
    }

    #[test]
    fn ctrl_9_returns_switch_tab_8() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('9'), &mut state, &[], 10, 80);
        assert!(matches!(action, Some(Action::SwitchTab(8))));
    }

    #[test]
    fn ctrl_5_returns_switch_tab_4() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('5'), &mut state, &[], 10, 80);
        assert!(matches!(action, Some(Action::SwitchTab(4))));
    }

    #[test]
    fn ctrl_c_still_quits() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('c'), &mut state, &[], 10, 80);
        assert!(matches!(action, Some(Action::Quit)));
    }
}
