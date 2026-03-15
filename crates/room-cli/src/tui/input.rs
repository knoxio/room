pub(super) use super::display::{
    byte_offset_at_display_pos, cursor_display_pos, find_at_context, next_word_end,
    prev_word_start, wrap_input_display,
};
use super::parse::parse_dm_input;
pub(super) use super::parse::{
    apply_backslash_enter, build_payload, normalize_paste, parse_kick_broadcast,
    parse_status_broadcast, parse_subscription_broadcast, seed_online_users_from_who,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::widgets::{ChoicePicker, CommandPalette, MentionPicker};

/// All mutable TUI input state. Pure data — no async context or I/O.
pub(super) struct InputState {
    pub(super) input: String,
    pub(super) cursor_pos: usize,
    pub(super) input_row_scroll: usize,
    pub(super) scroll_offset: usize,
    pub(super) palette: CommandPalette,
    pub(super) mention: MentionPicker,
    pub(super) choice: ChoicePicker,
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
            choice: ChoicePicker::new(),
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
    daemon_users: &[String],
    visible_count: usize,
    input_width: usize,
) -> Option<Action> {
    match key.code {
        KeyCode::Esc => handle_esc(state),
        KeyCode::Tab if state.choice.active => handle_tab_choice(state),
        KeyCode::Tab if state.mention.active => handle_tab_mention(state),
        KeyCode::Tab if state.palette.active => {
            complete_palette_selection(state, online_users, daemon_users);
        }
        KeyCode::Enter => return handle_enter(key, state, online_users, daemon_users),
        KeyCode::Up => handle_up(state, online_users, daemon_users, input_width),
        KeyCode::Down => handle_down(state, online_users, daemon_users, input_width),
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
            sync_mention_after_cursor_move(state, online_users, daemon_users);
        }
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
            state.cursor_pos = next_word_end(&state.input, state.cursor_pos);
            sync_mention_after_cursor_move(state, online_users, daemon_users);
        }
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => {
            handle_word_delete(state, online_users, daemon_users);
        }
        KeyCode::Char(c) => handle_char(c, state, online_users, daemon_users),
        KeyCode::Backspace => handle_backspace(state, online_users, daemon_users),
        KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
            state.cursor_pos = prev_word_start(&state.input, state.cursor_pos);
            sync_mention_after_cursor_move(state, online_users, daemon_users);
        }
        KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
            state.cursor_pos = next_word_end(&state.input, state.cursor_pos);
            sync_mention_after_cursor_move(state, online_users, daemon_users);
        }
        KeyCode::Left => handle_left(state, online_users, daemon_users),
        KeyCode::Right => handle_right(state, online_users, daemon_users),
        KeyCode::Home => {
            state.cursor_pos = 0;
            sync_mention_after_cursor_move(state, online_users, daemon_users);
        }
        KeyCode::End => {
            state.cursor_pos = state.input.len();
            sync_mention_after_cursor_move(state, online_users, daemon_users);
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

/// Dismiss choice picker, mention picker, command palette, or clear input on Esc.
fn handle_esc(state: &mut InputState) {
    if state.choice.active {
        state.choice.deactivate();
    } else if state.mention.active {
        state.mention.deactivate();
    } else if state.palette.active {
        state.palette.deactivate();
    } else if !state.input.is_empty() {
        state.input.clear();
        state.cursor_pos = 0;
        state.input_row_scroll = 0;
    }
}

/// Complete the currently selected choice on Tab.
fn handle_tab_choice(state: &mut InputState) {
    if let Some(value) = state.choice.selected_value() {
        let value = value.to_owned();
        let start = state.choice.value_start;
        let end = state.cursor_pos;
        state.input.replace_range(start..end, &value);
        state.cursor_pos = start + value.len();
        state.input_row_scroll = 0;
    }
    state.choice.deactivate();
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
fn handle_enter(
    key: KeyEvent,
    state: &mut InputState,
    online_users: &[String],
    daemon_users: &[String],
) -> Option<Action> {
    if state.choice.active {
        if let Some(value) = state.choice.selected_value() {
            let value = value.to_owned();
            let start = state.choice.value_start;
            let end = state.cursor_pos;
            state.input.replace_range(start..end, &value);
            state.cursor_pos = start + value.len();
            state.input_row_scroll = 0;
        }
        state.choice.deactivate();
    } else if state.mention.active {
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
        complete_palette_selection(state, online_users, daemon_users);
        // If no sub-picker was activated (mention or choice), the command is
        // fully formed — send it immediately instead of requiring a second Enter.
        if !state.mention.active && !state.choice.active && !state.input.is_empty() {
            let payload = build_payload(&state.input);
            state.input.clear();
            state.cursor_pos = 0;
            state.input_row_scroll = 0;
            state.scroll_offset = 0;
            return Some(Action::Send(payload));
        }
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
fn handle_up(
    state: &mut InputState,
    online_users: &[String],
    daemon_users: &[String],
    input_width: usize,
) {
    if state.choice.active {
        state.choice.move_up();
    } else if state.mention.active {
        state.mention.move_up();
    } else if state.palette.active {
        state.palette.move_up();
    } else {
        let (cur_row, cur_col) = cursor_display_pos(&state.input, state.cursor_pos, input_width);
        if cur_row > 0 {
            state.cursor_pos =
                byte_offset_at_display_pos(&state.input, cur_row - 1, cur_col, input_width);
            sync_mention_after_cursor_move(state, online_users, daemon_users);
        } else {
            state.scroll_offset = state.scroll_offset.saturating_add(1);
        }
    }
}

/// Navigate mention/palette picker downward, move cursor down in multi-line
/// input, or scroll the chat history.
fn handle_down(
    state: &mut InputState,
    online_users: &[String],
    daemon_users: &[String],
    input_width: usize,
) {
    if state.choice.active {
        state.choice.move_down();
    } else if state.mention.active {
        state.mention.move_down();
    } else if state.palette.active {
        state.palette.move_down();
    } else {
        let (cur_row, cur_col) = cursor_display_pos(&state.input, state.cursor_pos, input_width);
        let last_row = cursor_display_pos(&state.input, state.input.len(), input_width).0;
        if cur_row < last_row {
            state.cursor_pos =
                byte_offset_at_display_pos(&state.input, cur_row + 1, cur_col, input_width);
            sync_mention_after_cursor_move(state, online_users, daemon_users);
        } else if state.cursor_pos < state.input.len() {
            // On the last row but not at end of line — move to end first.
            state.cursor_pos = state.input.len();
            sync_mention_after_cursor_move(state, online_users, daemon_users);
        } else {
            state.scroll_offset = state.scroll_offset.saturating_sub(1);
        }
    }
}

/// Delete the previous word (Alt+Backspace).
fn handle_word_delete(state: &mut InputState, online_users: &[String], daemon_users: &[String]) {
    let word_start = prev_word_start(&state.input, state.cursor_pos);
    if word_start < state.cursor_pos {
        state.input.drain(word_start..state.cursor_pos);
        state.cursor_pos = word_start;
        sync_mention_after_delete(state, online_users, daemon_users);
        sync_palette_after_delete(state);
    }
}

/// Insert a character and update mention picker / command palette state.
fn handle_char(c: char, state: &mut InputState, online_users: &[String], daemon_users: &[String]) {
    state.input.insert(state.cursor_pos, c);
    state.cursor_pos += c.len_utf8();
    // Update choice picker filter when active.
    if state.choice.active {
        let query = &state.input[state.choice.value_start..state.cursor_pos];
        state.choice.update_filter(query);
        if state.choice.filtered.is_empty() {
            state.choice.deactivate();
        }
        return;
    }
    // Update mention picker state.
    if let Some((at_byte, query)) = find_at_context(&state.input, state.cursor_pos) {
        if state.mention.active || c == '@' {
            state
                .mention
                .activate(at_byte, online_users, daemon_users, query);
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
fn handle_backspace(state: &mut InputState, online_users: &[String], daemon_users: &[String]) {
    if state.cursor_pos > 0 {
        let prev = state.input[..state.cursor_pos]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        state.input.remove(prev);
        state.cursor_pos = prev;
        // Update choice picker on backspace.
        if state.choice.active {
            if state.cursor_pos < state.choice.value_start {
                state.choice.deactivate();
            } else {
                let query = &state.input[state.choice.value_start..state.cursor_pos];
                state.choice.update_filter(query);
                if state.choice.filtered.is_empty() {
                    state.choice.deactivate();
                }
            }
            return;
        }
        sync_mention_after_delete(state, online_users, daemon_users);
        sync_palette_after_delete(state);
    }
}

/// Move cursor one character to the left.
fn handle_left(state: &mut InputState, online_users: &[String], daemon_users: &[String]) {
    if state.cursor_pos > 0 {
        state.cursor_pos = state.input[..state.cursor_pos]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }
    sync_mention_after_cursor_move(state, online_users, daemon_users);
}

/// Move cursor one character to the right.
fn handle_right(state: &mut InputState, online_users: &[String], daemon_users: &[String]) {
    if state.cursor_pos < state.input.len() {
        let ch = state.input[state.cursor_pos..].chars().next().unwrap();
        state.cursor_pos += ch.len_utf8();
    }
    sync_mention_after_cursor_move(state, online_users, daemon_users);
}

/// Re-evaluate the mention picker after the cursor moved.
///
/// If the picker is active, checks the `@` context at the new cursor position.
/// Updates the filter if the context is still valid, or deactivates the picker
/// if the cursor has moved away from the `@` trigger.
fn sync_mention_after_cursor_move(
    state: &mut InputState,
    online_users: &[String],
    daemon_users: &[String],
) {
    if !state.mention.active {
        return;
    }
    if let Some((at_byte, query)) = find_at_context(&state.input, state.cursor_pos) {
        state.mention.at_byte = at_byte;
        state
            .mention
            .update_filter(online_users, daemon_users, query);
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
fn sync_mention_after_delete(
    state: &mut InputState,
    online_users: &[String],
    daemon_users: &[String],
) {
    if let Some((at_byte, query)) = find_at_context(&state.input, state.cursor_pos) {
        if state.mention.active {
            state.mention.at_byte = at_byte;
            state
                .mention
                .update_filter(online_users, daemon_users, query);
            if state.mention.filtered.is_empty() {
                state.mention.deactivate();
            }
        } else {
            state
                .mention
                .activate(at_byte, online_users, daemon_users, query);
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
fn complete_palette_selection(
    state: &mut InputState,
    online_users: &[String],
    daemon_users: &[String],
) {
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
            state
                .mention
                .activate(at_byte, online_users, daemon_users, "");
            if state.mention.filtered.is_empty() {
                state.mention.deactivate();
            }
        } else if has_choices {
            // Set input to "/<cmd> " and activate choice picker.
            let choices = state.palette.completions_at(&cmd_name, 0);
            let prefix = format!("/{cmd_name} ");
            let value_start = prefix.len();
            state.input = prefix;
            state.cursor_pos = value_start;
            state.input_row_scroll = 0;
            state.palette.deactivate();
            state.choice.activate(&cmd_name, choices, value_start, "");
            if state.choice.filtered.is_empty() {
                state.choice.deactivate();
            }
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

// Parse/display functions extracted to tui/parse.rs and tui/display.rs.

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Parse/display tests moved to parse.rs and display.rs.

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
        handle_key(make_key(KeyCode::Char('h')), &mut state, &[], &[], 10, 80);
        handle_key(make_key(KeyCode::Char('i')), &mut state, &[], &[], 10, 80);
        assert_eq!(state.input, "hi");
        assert_eq!(state.cursor_pos, 2);
    }

    #[test]
    fn enter_on_empty_input_does_nothing() {
        let mut state = InputState::new();
        let action = handle_key(make_key(KeyCode::Enter), &mut state, &[], &[], 10, 80);
        assert!(action.is_none());
        assert!(state.input.is_empty());
    }

    #[test]
    fn enter_on_nonempty_input_returns_send() {
        let mut state = InputState::new();
        state.input = "hello".to_owned();
        state.cursor_pos = 5;
        let action = handle_key(make_key(KeyCode::Enter), &mut state, &[], &[], 10, 80);
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
        handle_key(make_key(KeyCode::Backspace), &mut state, &[], &[], 10, 80);
        assert_eq!(state.input, "h");
        assert_eq!(state.cursor_pos, 1);
    }

    #[test]
    fn left_arrow_moves_cursor_back() {
        let mut state = InputState::new();
        state.input = "abc".to_owned();
        state.cursor_pos = 3;
        handle_key(make_key(KeyCode::Left), &mut state, &[], &[], 10, 80);
        assert_eq!(state.cursor_pos, 2);
    }

    #[test]
    fn right_arrow_moves_cursor_forward() {
        let mut state = InputState::new();
        state.input = "abc".to_owned();
        state.cursor_pos = 1;
        handle_key(make_key(KeyCode::Right), &mut state, &[], &[], 10, 80);
        assert_eq!(state.cursor_pos, 2);
    }

    #[test]
    fn home_moves_cursor_to_start() {
        let mut state = InputState::new();
        state.input = "hello".to_owned();
        state.cursor_pos = 5;
        handle_key(make_key(KeyCode::Home), &mut state, &[], &[], 10, 80);
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn end_moves_cursor_to_end() {
        let mut state = InputState::new();
        state.input = "hello".to_owned();
        state.cursor_pos = 0;
        handle_key(make_key(KeyCode::End), &mut state, &[], &[], 10, 80);
        assert_eq!(state.cursor_pos, 5);
    }

    /// Up on single-line (row 0) input scrolls message history.
    #[test]
    fn up_arrow_on_single_line_scrolls_history() {
        let mut state = InputState::new();
        state.scroll_offset = 0;
        handle_key(make_key(KeyCode::Up), &mut state, &[], &[], 10, 80);
        assert_eq!(state.scroll_offset, 1);
    }

    /// Down on single-line input (already at last row) decrements scroll.
    #[test]
    fn down_arrow_on_single_line_clamps_scroll_at_zero() {
        let mut state = InputState::new();
        state.scroll_offset = 0;
        handle_key(make_key(KeyCode::Down), &mut state, &[], &[], 10, 80);
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
        handle_key(make_key(KeyCode::Up), &mut state, &[], &[], 10, 80);
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
        handle_key(make_key(KeyCode::Down), &mut state, &[], &[], 10, 80);
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
        handle_key(make_key(KeyCode::Up), &mut state, &[], &[], 10, 80);
        assert_eq!(state.scroll_offset, 1);
        assert_eq!(state.cursor_pos, 2); // unchanged
    }

    /// Down on the last row mid-line moves cursor to end of input first.
    #[test]
    fn down_arrow_on_last_row_mid_line_moves_to_end() {
        let mut state = InputState::new();
        state.input = "hello\nworld".to_owned();
        state.cursor_pos = 8; // row 1 (last), mid-line
        state.scroll_offset = 4;
        handle_key(make_key(KeyCode::Down), &mut state, &[], &[], 10, 80);
        assert_eq!(state.cursor_pos, 11); // moved to end of input
        assert_eq!(state.scroll_offset, 4); // unchanged — no scroll yet
    }

    /// Down on the last row at end of input scrolls chat history.
    #[test]
    fn down_arrow_on_last_row_at_end_scrolls_history() {
        let mut state = InputState::new();
        state.input = "hello\nworld".to_owned();
        state.cursor_pos = 11; // end of input
        state.scroll_offset = 4;
        handle_key(make_key(KeyCode::Down), &mut state, &[], &[], 10, 80);
        assert_eq!(state.scroll_offset, 3); // decremented
        assert_eq!(state.cursor_pos, 11); // unchanged
    }

    /// Down on single-line input mid-line moves to end before scrolling.
    #[test]
    fn down_arrow_single_line_mid_line_moves_to_end() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 5; // mid-line
        state.scroll_offset = 3;
        handle_key(make_key(KeyCode::Down), &mut state, &[], &[], 10, 80);
        assert_eq!(state.cursor_pos, 11); // moved to end
        assert_eq!(state.scroll_offset, 3); // unchanged
    }

    /// Up clamps target column to the end of a shorter previous row.
    #[test]
    fn up_arrow_clamps_column_to_end_of_shorter_row() {
        let mut state = InputState::new();
        // row 0 = "hi" (2 chars), row 1 = "world" (5 chars)
        state.input = "hi\nworld".to_owned();
        // cursor at 'l' (index 6) on row 1, col 3
        state.cursor_pos = 6;
        handle_key(make_key(KeyCode::Up), &mut state, &[], &[], 10, 80);
        // row 0 only has col 0-2; col 3 clamps to end = byte 2 (position of '\n')
        assert_eq!(state.cursor_pos, 2);
    }

    #[test]
    fn page_up_scrolls_by_visible_count() {
        let mut state = InputState::new();
        handle_key(make_key(KeyCode::PageUp), &mut state, &[], &[], 10, 80);
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
        handle_key(make_key(KeyCode::Char('/')), &mut state, &[], &[], 10, 80);
        assert!(state.palette.active);
    }

    #[test]
    fn esc_deactivates_palette() {
        let mut state = InputState::new();
        state.palette.activate();
        handle_key(make_key(KeyCode::Esc), &mut state, &[], &[], 10, 80);
        assert!(!state.palette.active);
    }

    #[test]
    fn at_char_activates_mention_picker_when_users_present() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned(), "bob".to_owned()];
        handle_key(
            make_key(KeyCode::Char('@')),
            &mut state,
            &users,
            &[],
            10,
            80,
        );
        assert!(state.mention.active);
    }

    #[test]
    fn send_payload_is_valid_json_message() {
        let mut state = InputState::new();
        state.input = "hello world".to_owned();
        state.cursor_pos = 11;
        let action = handle_key(make_key(KeyCode::Enter), &mut state, &[], &[], 10, 80);
        if let Some(Action::Send(payload)) = action {
            let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(v["type"], "message");
            assert_eq!(v["content"], "hello world");
        } else {
            panic!("expected Action::Send");
        }
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
        handle_key(make_key(KeyCode::Esc), &mut state, &[], &[], 10, 80);
        assert!(state.input.is_empty());
        assert_eq!(state.cursor_pos, 0);
        assert_eq!(state.input_row_scroll, 0);
    }

    #[test]
    fn esc_on_empty_input_is_noop() {
        let mut state = InputState::new();
        handle_key(make_key(KeyCode::Esc), &mut state, &[], &[], 10, 80);
        assert!(state.input.is_empty());
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn esc_dismisses_palette_before_clearing_input() {
        let mut state = InputState::new();
        state.input = "/he".to_owned();
        state.cursor_pos = 3;
        state.palette.activate();
        handle_key(make_key(KeyCode::Esc), &mut state, &[], &[], 10, 80);
        // First Esc dismisses palette, input stays
        assert!(!state.palette.active);
        assert_eq!(state.input, "/he");
        // Second Esc clears input
        handle_key(make_key(KeyCode::Esc), &mut state, &[], &[], 10, 80);
        assert!(state.input.is_empty());
    }

    #[test]
    fn esc_dismisses_mention_before_clearing_input() {
        let mut state = InputState::new();
        state.input = "@al".to_owned();
        state.cursor_pos = 3;
        let users = vec!["alice".to_owned()];
        state.mention.activate(0, &users, &[], "al");
        handle_key(make_key(KeyCode::Esc), &mut state, &users, &[], 10, 80);
        // First Esc dismisses mention picker
        assert!(!state.mention.active);
        assert_eq!(state.input, "@al");
        // Second Esc clears input
        handle_key(make_key(KeyCode::Esc), &mut state, &users, &[], 10, 80);
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
        handle_key(
            make_key(KeyCode::Char('@')),
            &mut state,
            &users,
            &[],
            10,
            80,
        );
        assert!(state.mention.active);
        // Move cursor left — now before the '@', picker should deactivate.
        handle_key(make_key(KeyCode::Left), &mut state, &users, &[], 10, 80);
        assert!(!state.mention.active);
    }

    #[test]
    fn mention_picker_deactivates_on_home() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        // Type "hi @" to activate mention picker.
        for c in "hi @".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(state.mention.active);
        // Home moves cursor to 0 — well before the '@'.
        handle_key(make_key(KeyCode::Home), &mut state, &users, &[], 10, 80);
        assert!(!state.mention.active);
    }

    #[test]
    fn mention_picker_updates_filter_on_right_arrow() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned(), "bob".to_owned()];
        // Type "@ali" — picker active, filtered to alice.
        for c in "@ali".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(state.mention.active);
        assert_eq!(state.mention.filtered.len(), 1);
        // Move cursor left twice (to "@a|li" — cursor between 'a' and 'l').
        handle_key(make_key(KeyCode::Left), &mut state, &users, &[], 10, 80);
        handle_key(make_key(KeyCode::Left), &mut state, &users, &[], 10, 80);
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
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(!state.mention.active);
        // Re-type "@b" to activate again.
        for c in "@b".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
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
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(state.mention.active);
        // Move left to "@a|l" then right back to "@al|".
        handle_key(make_key(KeyCode::Left), &mut state, &users, &[], 10, 80);
        assert!(state.mention.active);
        handle_key(make_key(KeyCode::Right), &mut state, &users, &[], 10, 80);
        assert!(state.mention.active);
    }

    #[test]
    fn mention_picker_deactivates_on_alt_left_past_at() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        // Type "hi @al" — picker active.
        for c in "hi @al".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(state.mention.active);
        // Alt+Left jumps to word start — past the '@'.
        handle_key(
            make_key_mod(KeyCode::Left, KeyModifiers::ALT),
            &mut state,
            &users,
            &[],
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
            handle_key(make_key(KeyCode::Char(c)), &mut state, &[], &[], 10, 80);
        }
        assert!(state.palette.active);
        // Type "zzz" — no matches, palette auto-dismisses.
        for c in "zzz".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &[], &[], 10, 80);
        }
        assert!(!state.palette.active);
        assert_eq!(state.input, "/hezzz");
        // Backspace 3 times to get back to "/he" — palette should reactivate.
        for _ in 0..3 {
            handle_key(make_key(KeyCode::Backspace), &mut state, &[], &[], 10, 80);
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
            handle_key(make_key(KeyCode::Char(c)), &mut state, &[], &[], 10, 80);
        }
        assert!(!state.palette.active);
        // Backspace to "/zz" — still no matches, stays dismissed.
        handle_key(make_key(KeyCode::Backspace), &mut state, &[], &[], 10, 80);
        assert!(!state.palette.active);
    }

    #[test]
    fn palette_deactivates_when_slash_is_deleted() {
        let mut state = InputState::new();
        // Type "/" — palette activates.
        handle_key(make_key(KeyCode::Char('/')), &mut state, &[], &[], 10, 80);
        assert!(state.palette.active);
        // Backspace removes "/" — palette deactivates.
        handle_key(make_key(KeyCode::Backspace), &mut state, &[], &[], 10, 80);
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
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(state.mention.active);
        // Type "zzz" — no matches, picker auto-dismisses.
        for c in "zzz".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(!state.mention.active);
        assert_eq!(state.input, "@alizzz");
        // Backspace 3 times to get back to "@ali" — picker should reactivate.
        for _ in 0..3 {
            handle_key(
                make_key(KeyCode::Backspace),
                &mut state,
                &users,
                &[],
                10,
                80,
            );
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
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(!state.mention.active);
        // Backspace to "@zz" — still no user matches "zz".
        handle_key(
            make_key(KeyCode::Backspace),
            &mut state,
            &users,
            &[],
            10,
            80,
        );
        assert!(!state.mention.active);
    }

    // ── complete_palette_selection tests (#257) ───────────────────────────────

    #[test]
    fn tab_on_dm_activates_mention_picker() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned(), "bob".to_owned()];
        // Type "/dm" to activate and filter palette
        for c in "/dm".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(state.palette.active);
        // Tab to complete — should set input to "/dm " and activate mention picker
        handle_key(make_key(KeyCode::Tab), &mut state, &users, &[], 10, 80);
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
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(state.palette.active);
        handle_key(make_key(KeyCode::Tab), &mut state, &users, &[], 10, 80);
        assert_eq!(state.input, "/kick ");
        assert!(state.mention.active);
    }

    #[test]
    fn tab_on_stats_sets_short_prefix_for_choices() {
        let mut state = InputState::new();
        let users: Vec<String> = vec![];
        for c in "/stats".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(state.palette.active);
        handle_key(make_key(KeyCode::Tab), &mut state, &users, &[], 10, 80);
        // stats has Choice param — should set "/stats " and activate choice picker
        assert_eq!(state.input, "/stats ");
        assert!(!state.palette.active);
        assert!(!state.mention.active);
        assert!(
            state.choice.active,
            "choice picker should activate for Choice params"
        );
        assert!(
            !state.choice.filtered.is_empty(),
            "choice picker should have filtered values"
        );
    }

    #[test]
    fn tab_on_who_fills_full_usage() {
        let mut state = InputState::new();
        let users: Vec<String> = vec![];
        for c in "/who".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        handle_key(make_key(KeyCode::Tab), &mut state, &users, &[], 10, 80);
        // who has no params — fills in the full usage string
        assert_eq!(state.input, "/who");
        assert!(!state.palette.active);
    }

    #[test]
    fn enter_on_dm_activates_mention_picker() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        for c in "/dm".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        // Enter also completes palette selection
        handle_key(make_key(KeyCode::Enter), &mut state, &users, &[], 10, 80);
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
        let action = handle_key(make_ctrl_key('n'), &mut state, &[], &[], 10, 80);
        assert!(matches!(action, Some(Action::NextTab)));
    }

    #[test]
    fn ctrl_p_returns_prev_tab_action() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('p'), &mut state, &[], &[], 10, 80);
        assert!(matches!(action, Some(Action::PrevTab)));
    }

    #[test]
    fn ctrl_1_returns_switch_tab_0() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('1'), &mut state, &[], &[], 10, 80);
        assert!(matches!(action, Some(Action::SwitchTab(0))));
    }

    #[test]
    fn ctrl_9_returns_switch_tab_8() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('9'), &mut state, &[], &[], 10, 80);
        assert!(matches!(action, Some(Action::SwitchTab(8))));
    }

    #[test]
    fn ctrl_5_returns_switch_tab_4() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('5'), &mut state, &[], &[], 10, 80);
        assert!(matches!(action, Some(Action::SwitchTab(4))));
    }

    #[test]
    fn ctrl_c_still_quits() {
        let mut state = InputState::new();
        let action = handle_key(make_ctrl_key('c'), &mut state, &[], &[], 10, 80);
        assert!(matches!(action, Some(Action::Quit)));
    }

    // ── ChoicePicker integration tests ──────────────────────────────────────

    /// Helper: type a command and Tab to activate the choice picker.
    fn activate_choice_picker(cmd: &str) -> InputState {
        let mut state = InputState::new();
        let users: Vec<String> = vec![];
        for c in cmd.chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        handle_key(make_key(KeyCode::Tab), &mut state, &users, &[], 10, 80);
        state
    }

    #[test]
    fn choice_picker_activates_on_subscribe_tab() {
        let state = activate_choice_picker("/subscribe");
        assert!(state.choice.active);
        assert_eq!(state.input, "/subscribe ");
        assert!(state.choice.filtered.contains(&"full".to_owned()));
        assert!(state.choice.filtered.contains(&"mentions_only".to_owned()));
    }

    #[test]
    fn choice_picker_tab_completes_selection() {
        let mut state = activate_choice_picker("/subscribe");
        assert!(state.choice.active);
        // Default selection is first item
        handle_key(make_key(KeyCode::Tab), &mut state, &[], &[], 10, 80);
        assert!(!state.choice.active);
        // Input should contain the selected value
        assert!(
            state.input.starts_with("/subscribe "),
            "input should start with '/subscribe '"
        );
        assert!(
            state.input.len() > "/subscribe ".len(),
            "input should contain the completed value"
        );
    }

    #[test]
    fn choice_picker_enter_completes_selection() {
        let mut state = activate_choice_picker("/subscribe");
        assert!(state.choice.active);
        handle_key(make_key(KeyCode::Enter), &mut state, &[], &[], 10, 80);
        assert!(!state.choice.active);
        assert!(state.input.starts_with("/subscribe "));
    }

    #[test]
    fn choice_picker_up_down_navigates() {
        let mut state = activate_choice_picker("/subscribe");
        assert!(state.choice.active);
        assert_eq!(state.choice.selected, 0);
        handle_key(make_key(KeyCode::Down), &mut state, &[], &[], 10, 80);
        assert_eq!(state.choice.selected, 1);
        handle_key(make_key(KeyCode::Up), &mut state, &[], &[], 10, 80);
        assert_eq!(state.choice.selected, 0);
    }

    #[test]
    fn choice_picker_esc_dismisses() {
        let mut state = activate_choice_picker("/subscribe");
        assert!(state.choice.active);
        handle_key(make_key(KeyCode::Esc), &mut state, &[], &[], 10, 80);
        assert!(!state.choice.active);
    }

    #[test]
    fn choice_picker_typing_filters() {
        let mut state = activate_choice_picker("/subscribe");
        assert!(state.choice.active);
        let initial_count = state.choice.filtered.len();
        // Type 'f' to filter to "full"
        handle_key(make_key(KeyCode::Char('f')), &mut state, &[], &[], 10, 80);
        assert!(state.choice.active);
        assert!(state.choice.filtered.len() < initial_count);
        assert!(state.choice.filtered.contains(&"full".to_owned()));
    }

    #[test]
    fn choice_picker_typing_no_match_deactivates() {
        let mut state = activate_choice_picker("/subscribe");
        assert!(state.choice.active);
        for c in "zzz".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &[], &[], 10, 80);
        }
        assert!(
            !state.choice.active,
            "choice picker should deactivate when no matches"
        );
    }

    #[test]
    fn choice_picker_backspace_updates_filter() {
        let mut state = activate_choice_picker("/subscribe");
        assert!(state.choice.active);
        // Type 'f' to filter
        handle_key(make_key(KeyCode::Char('f')), &mut state, &[], &[], 10, 80);
        let filtered_count = state.choice.filtered.len();
        // Backspace restores full list
        handle_key(make_key(KeyCode::Backspace), &mut state, &[], &[], 10, 80);
        assert!(state.choice.active);
        assert!(state.choice.filtered.len() >= filtered_count);
    }

    #[test]
    fn choice_picker_backspace_past_value_start_deactivates() {
        let mut state = activate_choice_picker("/subscribe");
        assert!(state.choice.active);
        // Backspace into the command prefix should deactivate
        handle_key(make_key(KeyCode::Backspace), &mut state, &[], &[], 10, 80);
        assert!(
            !state.choice.active,
            "choice picker should deactivate when cursor moves before value_start"
        );
    }

    #[test]
    fn choice_picker_not_active_for_who() {
        let state = activate_choice_picker("/who");
        assert!(
            !state.choice.active,
            "who has no Choice params — picker should not activate"
        );
    }

    #[test]
    fn choice_picker_not_active_for_dm() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        for c in "/dm".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        handle_key(make_key(KeyCode::Tab), &mut state, &users, &[], 10, 80);
        // dm has Username param, not Choice — mention picker activates, not choice
        assert!(
            !state.choice.active,
            "choice picker should not activate for Username params"
        );
        assert!(
            state.mention.active,
            "mention picker should activate for Username params"
        );
    }

    // ── Ctrl+D behavior (raw-mode TUI) ─────────────────────────────────────

    #[test]
    fn ctrl_d_does_not_quit() {
        // In raw-mode TUI, Ctrl+D is not an exit signal (Ctrl+C is used
        // instead). Ctrl+D falls through to handle_char which inserts 'd'.
        let mut state = InputState::new();
        let action = handle_key(
            make_key_mod(KeyCode::Char('d'), KeyModifiers::CONTROL),
            &mut state,
            &[],
            &[],
            10,
            80,
        );
        assert!(action.is_none(), "Ctrl+D should not trigger Quit");
    }

    // ── Tab switching (Ctrl+N / Ctrl+P / Ctrl+1-9) ─────────────────────────

    #[test]
    fn ctrl_n_returns_next_tab() {
        let mut state = InputState::new();
        let action = handle_key(
            make_key_mod(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &mut state,
            &[],
            &[],
            10,
            80,
        );
        assert!(matches!(action, Some(Action::NextTab)));
    }

    #[test]
    fn ctrl_p_returns_prev_tab() {
        let mut state = InputState::new();
        let action = handle_key(
            make_key_mod(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut state,
            &[],
            &[],
            10,
            80,
        );
        assert!(matches!(action, Some(Action::PrevTab)));
    }

    #[test]
    fn ctrl_1_returns_switch_tab_zero() {
        let mut state = InputState::new();
        let action = handle_key(
            make_key_mod(KeyCode::Char('1'), KeyModifiers::CONTROL),
            &mut state,
            &[],
            &[],
            10,
            80,
        );
        assert!(matches!(action, Some(Action::SwitchTab(0))));
    }

    #[test]
    fn ctrl_9_returns_switch_tab_eight() {
        let mut state = InputState::new();
        let action = handle_key(
            make_key_mod(KeyCode::Char('9'), KeyModifiers::CONTROL),
            &mut state,
            &[],
            &[],
            10,
            80,
        );
        assert!(matches!(action, Some(Action::SwitchTab(8))));
    }

    // ── PageUp / PageDown scroll ────────────────────────────────────────────

    #[test]
    fn page_up_increases_scroll_offset() {
        let mut state = InputState::new();
        state.scroll_offset = 0;
        handle_key(make_key(KeyCode::PageUp), &mut state, &[], &[], 10, 80);
        assert_eq!(state.scroll_offset, 10, "PageUp should add visible_count");
    }

    #[test]
    fn page_down_decreases_scroll_offset() {
        let mut state = InputState::new();
        state.scroll_offset = 15;
        handle_key(make_key(KeyCode::PageDown), &mut state, &[], &[], 10, 80);
        assert_eq!(
            state.scroll_offset, 5,
            "PageDown should subtract visible_count"
        );
    }

    #[test]
    fn page_down_saturates_at_zero() {
        let mut state = InputState::new();
        state.scroll_offset = 3;
        handle_key(make_key(KeyCode::PageDown), &mut state, &[], &[], 10, 80);
        assert_eq!(state.scroll_offset, 0, "PageDown should not go below 0");
    }

    // ── Palette Enter sends immediately for no-param commands (#720) ─────────

    #[test]
    fn palette_enter_sends_immediately_for_no_param_command() {
        let mut state = InputState::new();
        // Type "/who" — palette should be active
        for c in "/who".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &[], &[], 10, 80);
        }
        assert!(state.palette.active, "palette should be active after /who");
        // Press Enter — should complete and send in one keystroke
        let action = handle_key(make_key(KeyCode::Enter), &mut state, &[], &[], 10, 80);
        assert!(
            matches!(action, Some(Action::Send(_))),
            "Enter on no-param palette command should send immediately"
        );
        assert!(state.input.is_empty(), "input should be cleared after send");
    }

    #[test]
    fn palette_enter_does_not_send_when_mention_picker_activates() {
        let mut state = InputState::new();
        let users = vec!["alice".to_owned()];
        for c in "/dm".chars() {
            handle_key(make_key(KeyCode::Char(c)), &mut state, &users, &[], 10, 80);
        }
        assert!(state.palette.active, "palette should be active after /dm");
        // Press Enter — should activate mention picker, not send
        let action = handle_key(make_key(KeyCode::Enter), &mut state, &users, &[], 10, 80);
        assert!(action.is_none(), "Enter on /dm should not send immediately");
        assert!(state.mention.active, "mention picker should be active");
    }
}
