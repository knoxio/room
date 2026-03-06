use std::io;

use unicode_width::UnicodeWidthChar;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Terminal,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

mod render;
mod widgets;

use crate::message::Message;
use render::{find_view_start, format_message, user_color};
use widgets::{CommandPalette, MentionPicker, PALETTE_COMMANDS};

/// Maximum visible content lines in the input box before it stops growing.
const MAX_INPUT_LINES: usize = 6;

pub async fn run(
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    room_id: &str,
    username: &str,
    history_lines: usize,
) -> anyhow::Result<()> {
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Message>();
    let username_owned = username.to_owned();

    // Spawn socket-reader task: buffers history until our join event,
    // then streams live messages.
    tokio::spawn(async move {
        let mut reader = reader;
        let mut history_buf: Vec<Message> = Vec::new();
        let mut joined = false;
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let Ok(msg) = serde_json::from_str::<Message>(trimmed) else {
                        continue;
                    };

                    if joined {
                        let _ = msg_tx.send(msg);
                    } else {
                        let is_own_join =
                            matches!(&msg, Message::Join { user, .. } if user == &username_owned);
                        if is_own_join {
                            joined = true;
                            // Flush last N history entries then the join event
                            let start = history_buf.len().saturating_sub(history_lines);
                            for h in history_buf.drain(start..) {
                                let _ = msg_tx.send(h);
                            }
                            let _ = msg_tx.send(msg);
                        } else {
                            history_buf.push(msg);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut messages: Vec<Message> = Vec::new();
    let mut online_users: Vec<String> = Vec::new();
    let mut input = String::new();
    let mut cursor_pos: usize = 0; // byte index into `input`, always on a char boundary
    let mut input_row_scroll: usize = 0; // vertical scroll within the input box
    let mut scroll_offset: usize = 0;
    let mut palette = CommandPalette::new(PALETTE_COMMANDS);
    let mut mention = MentionPicker::new();
    let mut result: anyhow::Result<()> = Ok(());

    // Seed online_users immediately so @mention autocomplete works for users
    // who were already connected before we joined.
    let who_payload = build_payload("/who");
    write_half
        .write_all(format!("{who_payload}\n").as_bytes())
        .await?;

    'main: loop {
        // Drain pending messages from the socket reader.
        // Break 'main when the broker disconnects (sender dropped).
        loop {
            match msg_rx.try_recv() {
                Ok(msg) => {
                    match &msg {
                        Message::Join { user, .. } if !online_users.contains(user) => {
                            online_users.push(user.clone());
                        }
                        Message::Leave { user, .. } => {
                            online_users.retain(|u| u != user);
                        }
                        // Seed from message senders so @mention works for users who connected
                        // via poll/send (no persistent connection, not in the status_map).
                        Message::Message { user, .. } if !online_users.contains(user) => {
                            online_users.push(user.clone());
                        }
                        // Parse the /who response to seed the authoritative user list.
                        Message::System { user, content, .. } if user == "broker" => {
                            seed_online_users_from_who(content, &mut online_users);
                        }
                        _ => {}
                    }
                    messages.push(msg);
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break 'main,
            }
        }

        let term_area = terminal.size()?;
        // Input content width is terminal width minus the two border columns.
        let input_content_width = term_area.width.saturating_sub(2) as usize;

        // Compute wrapped display rows for the input and the cursor position within them.
        let input_display_rows = wrap_input_display(&input, input_content_width);
        let total_input_rows = input_display_rows.len();
        let visible_input_lines = total_input_rows.min(MAX_INPUT_LINES);
        // +2 for top and bottom borders; minimum 3 (1 content line + 2 borders).
        let input_box_height = (visible_input_lines + 2) as u16;

        let (cursor_row, cursor_col) = cursor_display_pos(&input, cursor_pos, input_content_width);

        // Adjust vertical scroll so the cursor stays visible.
        if cursor_row < input_row_scroll {
            input_row_scroll = cursor_row;
        }
        if visible_input_lines > 0 && cursor_row >= input_row_scroll + visible_input_lines {
            input_row_scroll = cursor_row + 1 - visible_input_lines;
        }

        let content_width = term_area.width.saturating_sub(2) as usize;
        let visible_count = term_area
            .height
            .saturating_sub(input_box_height)
            .saturating_sub(2) as usize;

        let msg_texts: Vec<Text<'static>> = messages
            .iter()
            .map(|m| format_message(m, content_width))
            .collect();

        let heights: Vec<usize> = msg_texts.iter().map(|t| t.lines.len().max(1)).collect();
        let total_lines: usize = heights.iter().sum();

        // Clamp scroll offset so it can't exceed scrollable range
        scroll_offset = scroll_offset.min(total_lines.saturating_sub(visible_count));

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(input_box_height)])
                .split(f.area());

            // actual visible rows from layout (overrides approximation)
            let actual_visible = chunks[0].height.saturating_sub(2) as usize;

            let view_bottom = total_lines.saturating_sub(scroll_offset);
            let view_top = view_bottom.saturating_sub(actual_visible);

            let (start_msg_idx, skip_first) = find_view_start(&heights, view_top);

            let visible: Vec<ListItem> = msg_texts[start_msg_idx..]
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    if i == 0 && skip_first > 0 {
                        ListItem::new(Text::from(text.lines[skip_first..].to_vec()))
                    } else {
                        ListItem::new(text.clone())
                    }
                })
                .collect();

            let title = if scroll_offset > 0 {
                format!(" {room_id} [↑ {scroll_offset} lines] ")
            } else {
                format!(" {room_id} ")
            };
            let msg_list = List::new(visible).block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            f.render_widget(msg_list, chunks[0]);

            // Render only the visible slice of wrapped input rows.
            let end = (input_row_scroll + visible_input_lines).min(total_input_rows);
            let display_text = input_display_rows[input_row_scroll..end].join("\n");

            let input_widget = Paragraph::new(display_text)
                .block(
                    Block::default()
                        .title(format!(" {username} "))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                )
                .style(Style::default().fg(Color::White));
            f.render_widget(input_widget, chunks[1]);

            // Place terminal cursor inside the input box.
            let visible_cursor_row = cursor_row - input_row_scroll;
            let cursor_x = chunks[1].x + 1 + cursor_col as u16;
            let cursor_y = chunks[1].y + 1 + visible_cursor_row as u16;
            f.set_cursor_position((cursor_x, cursor_y));

            // Render the command palette popup above the input box when active.
            if palette.active && !palette.filtered.is_empty() {
                let palette_items: Vec<ListItem> = palette
                    .filtered
                    .iter()
                    .enumerate()
                    .map(|(row, &idx)| {
                        let item = &palette.commands[idx];
                        let style = if row == palette.selected {
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled(
                                format!("{:<16}", item.usage),
                                style.add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("  {}", item.description),
                                if row == palette.selected {
                                    Style::default().fg(Color::Black).bg(Color::Cyan)
                                } else {
                                    Style::default().fg(Color::DarkGray)
                                },
                            ),
                        ]))
                    })
                    .collect();

                let popup_height = (palette.filtered.len() as u16 + 2).min(chunks[0].height);
                let popup_y = chunks[1].y.saturating_sub(popup_height);
                let popup_rect = Rect {
                    x: chunks[1].x,
                    y: popup_y,
                    width: chunks[1].width,
                    height: popup_height,
                };

                f.render_widget(Clear, popup_rect);
                let palette_list = List::new(palette_items).block(
                    Block::default()
                        .title(" commands ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                );
                f.render_widget(palette_list, popup_rect);
            }

            // Render the mention picker popup above the cursor when active.
            if mention.active && !mention.filtered.is_empty() {
                let mention_items: Vec<ListItem> = mention
                    .filtered
                    .iter()
                    .enumerate()
                    .map(|(row, user)| {
                        let style = if row == mention.selected {
                            Style::default()
                                .fg(Color::Black)
                                .bg(user_color(user))
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(user_color(user))
                        };
                        ListItem::new(Line::from(Span::styled(format!("@{user}"), style)))
                    })
                    .collect();

                let popup_height = (mention.filtered.len() as u16 + 2).min(chunks[0].height);
                let popup_y = chunks[1].y.saturating_sub(popup_height);
                let max_width = mention
                    .filtered
                    .iter()
                    .map(|u| u.len() + 1) // '@' + username
                    .max()
                    .unwrap_or(8) as u16
                    + 4; // borders + padding
                let popup_width = max_width.min(chunks[1].width / 2).max(8);
                let popup_x = cursor_x
                    .saturating_sub(1)
                    .min(chunks[1].x + chunks[1].width.saturating_sub(popup_width));
                let popup_rect = Rect {
                    x: popup_x,
                    y: popup_y,
                    width: popup_width,
                    height: popup_height,
                };

                f.render_widget(Clear, popup_rect);
                let mention_list = List::new(mention_items).block(
                    Block::default()
                        .title(" @ ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow)),
                );
                f.render_widget(mention_list, popup_rect);
            }
        })?;

        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Esc => {
                        if mention.active {
                            mention.deactivate();
                        } else if palette.active {
                            palette.deactivate();
                        }
                    }
                    KeyCode::Tab if mention.active => {
                        if let Some(user) = mention.selected_user() {
                            let user = user.to_owned();
                            let replacement = format!("@{user} ");
                            let query_end = cursor_pos;
                            let at_start = mention.at_byte;
                            input.replace_range(at_start..query_end, &replacement);
                            cursor_pos = at_start + replacement.len();
                            input_row_scroll = 0;
                        }
                        mention.deactivate();
                    }
                    KeyCode::Tab if palette.active => {
                        // Complete with selected command usage, replacing input.
                        if let Some(usage) = palette.selected_usage() {
                            input = usage.to_owned();
                            cursor_pos = input.len();
                            input_row_scroll = 0;
                        }
                        palette.deactivate();
                    }
                    KeyCode::Enter => {
                        if mention.active {
                            if let Some(user) = mention.selected_user() {
                                let user = user.to_owned();
                                let replacement = format!("@{user} ");
                                let query_end = cursor_pos;
                                let at_start = mention.at_byte;
                                input.replace_range(at_start..query_end, &replacement);
                                cursor_pos = at_start + replacement.len();
                                input_row_scroll = 0;
                            }
                            mention.deactivate();
                        } else if palette.active {
                            // Complete and deactivate — user presses Enter to confirm selection.
                            if let Some(usage) = palette.selected_usage() {
                                input = usage.to_owned();
                                cursor_pos = input.len();
                                input_row_scroll = 0;
                            }
                            palette.deactivate();
                        } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                            // Shift+Enter: insert a newline at the cursor.
                            input.insert(cursor_pos, '\n');
                            cursor_pos += 1;
                        } else if let Some(new_pos) = apply_backslash_enter(&mut input, cursor_pos)
                        {
                            // Backslash + Enter: strip the trailing '\' and insert a newline.
                            cursor_pos = new_pos;
                        } else if !input.is_empty() {
                            let payload = build_payload(&input);
                            input.clear();
                            cursor_pos = 0;
                            input_row_scroll = 0;
                            scroll_offset = 0;
                            if let Err(e) = write_half
                                .write_all(format!("{payload}\n").as_bytes())
                                .await
                            {
                                result = Err(e.into());
                                break 'main;
                            }
                        }
                    }
                    KeyCode::Up => {
                        if mention.active {
                            mention.move_up();
                        } else if palette.active {
                            palette.move_up();
                        } else {
                            scroll_offset = scroll_offset.saturating_add(1);
                        }
                    }
                    KeyCode::Down => {
                        if mention.active {
                            mention.move_down();
                        } else if palette.active {
                            palette.move_down();
                        } else {
                            scroll_offset = scroll_offset.saturating_sub(1);
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break 'main;
                    }
                    KeyCode::Char(c) => {
                        input.insert(cursor_pos, c);
                        cursor_pos += c.len_utf8();
                        // Update mention picker state.
                        if let Some((at_byte, query)) = find_at_context(&input, cursor_pos) {
                            if mention.active || c == '@' {
                                mention.activate(at_byte, &online_users, query);
                                if mention.filtered.is_empty() {
                                    mention.deactivate();
                                }
                            }
                        } else {
                            mention.deactivate();
                        }
                        // Activate slash palette when `/` is typed into an empty input.
                        if c == '/' && input == "/" {
                            palette.activate();
                        } else if palette.active {
                            // Update autocomplete filter: query is everything after the leading `/`.
                            if let Some(query) = input.strip_prefix('/') {
                                palette.update_filter(query);
                                if palette.filtered.is_empty() {
                                    palette.deactivate();
                                }
                            } else {
                                palette.deactivate();
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        if cursor_pos > 0 {
                            let prev = input[..cursor_pos]
                                .char_indices()
                                .next_back()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            input.remove(prev);
                            cursor_pos = prev;
                            // Sync mention picker after deletion.
                            if mention.active {
                                if let Some((at_byte, query)) = find_at_context(&input, cursor_pos)
                                {
                                    mention.at_byte = at_byte;
                                    mention.update_filter(&online_users, query);
                                    if mention.filtered.is_empty() {
                                        mention.deactivate();
                                    }
                                } else {
                                    mention.deactivate();
                                }
                            }
                            // Sync palette state after deletion.
                            if palette.active {
                                if input.is_empty() {
                                    palette.deactivate();
                                } else if let Some(query) = input.strip_prefix('/') {
                                    palette.update_filter(query);
                                    if palette.filtered.is_empty() {
                                        palette.deactivate();
                                    }
                                } else {
                                    palette.deactivate();
                                }
                            }
                        }
                    }
                    KeyCode::Left => {
                        if cursor_pos > 0 {
                            cursor_pos = input[..cursor_pos]
                                .char_indices()
                                .next_back()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                        }
                    }
                    KeyCode::Right => {
                        if cursor_pos < input.len() {
                            let ch = input[cursor_pos..].chars().next().unwrap();
                            cursor_pos += ch.len_utf8();
                        }
                    }
                    KeyCode::Home => {
                        cursor_pos = 0;
                    }
                    KeyCode::End => {
                        cursor_pos = input.len();
                    }
                    KeyCode::PageUp => {
                        scroll_offset = scroll_offset.saturating_add(visible_count);
                    }
                    KeyCode::PageDown => {
                        scroll_offset = scroll_offset.saturating_sub(visible_count);
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        // Drain any messages that arrived during the poll.
        // Break 'main when the broker disconnects (sender dropped).
        loop {
            match msg_rx.try_recv() {
                Ok(msg) => {
                    match &msg {
                        Message::Join { user, .. } if !online_users.contains(user) => {
                            online_users.push(user.clone());
                        }
                        Message::Leave { user, .. } => {
                            online_users.retain(|u| u != user);
                        }
                        // Seed from message senders so @mention works for users who connected
                        // via poll/send (no persistent connection, not in the status_map).
                        Message::Message { user, .. } if !online_users.contains(user) => {
                            online_users.push(user.clone());
                        }
                        Message::System { user, content, .. } if user == "broker" => {
                            seed_online_users_from_who(content, &mut online_users);
                        }
                        _ => {}
                    }
                    messages.push(msg);
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break 'main,
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Wrap input text for display in the input box.
///
/// Splits on `\n` (explicit newlines from Shift+Enter), then wraps each
/// logical line character-by-character at `width` display columns using
/// Unicode display widths. Returns the flat list of display rows.
///
/// If `width` is 0, returns each logical line unsplit.
fn wrap_input_display(input: &str, width: usize) -> Vec<String> {
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
fn cursor_display_pos(input: &str, cursor_pos: usize, width: usize) -> (usize, usize) {
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


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── seed_online_users_from_who tests ─────────────────────────────────────

    #[test]
    fn seed_who_populates_users() {
        let mut users = Vec::new();
        seed_online_users_from_who("online — alice, bob, charlie", &mut users);
        assert_eq!(users, ["alice", "bob", "charlie"]);
    }

    #[test]
    fn seed_who_strips_status_suffix() {
        let mut users = Vec::new();
        seed_online_users_from_who("online — alice: away, bob: coding", &mut users);
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
        seed_online_users_from_who("online — alice, bob", &mut users);
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

    // ── wrap_input_display and cursor_display_pos tests ───────────────────────

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
        // "abcde" is exactly 5 chars, width 5 — no wrap needed.
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
        // "abcde\nfghij" with width 3 → each logical line wraps independently.
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
        // '中' has display width 2. With width 4, "中中中" should split after
        // the second character (col would reach 4 exactly), third starts new row.
        let rows = wrap_input_display("中中中", 4);
        assert_eq!(rows, vec!["中中", "中"]);
    }

    // ── cursor_display_pos ──────────────────────────────────────────────────

    #[test]
    fn cursor_at_start_of_empty_input() {
        assert_eq!(cursor_display_pos("", 0, 10), (0, 0));
    }

    #[test]
    fn cursor_at_end_of_single_line() {
        // "hello" fits in width 10; cursor at byte 5 → col 5 row 0.
        assert_eq!(cursor_display_pos("hello", 5, 10), (0, 5));
    }

    #[test]
    fn cursor_mid_single_line() {
        assert_eq!(cursor_display_pos("hello", 2, 10), (0, 2));
    }

    #[test]
    fn cursor_wraps_to_second_row() {
        // "abcdef" width 5: 'f' is on row 1 col 0.
        assert_eq!(cursor_display_pos("abcdef", 5, 5), (1, 0));
    }

    #[test]
    fn cursor_after_explicit_newline() {
        // "hi\n" — cursor after the '\n' is at (1, 0).
        assert_eq!(cursor_display_pos("hi\n", 3, 20), (1, 0));
    }

    #[test]
    fn cursor_on_second_explicit_line() {
        // "foo\nbar" with width 10: cursor at byte 7 (end) is (1, 3).
        assert_eq!(cursor_display_pos("foo\nbar", 7, 10), (1, 3));
    }

    #[test]
    fn cursor_explicit_newline_combined_with_wrap() {
        // "abc\ndefgh" width 3: "abc" → row 0, "def" → row 1, "gh" → row 2.
        // cursor at byte 9 (end, 'h') → (2, 2).
        let s = "abc\ndefgh";
        assert_eq!(cursor_display_pos(s, s.len(), 3), (2, 2));
    }

    #[test]
    fn cursor_width_zero_no_wrapping() {
        // width=0 disables wrapping; col just accumulates.
        assert_eq!(cursor_display_pos("hello world", 5, 0), (0, 5));
    }

    #[test]
    fn cursor_wide_char_advances_by_display_width() {
        // '中' = 3 bytes, display width 2. "中中" with width 4 fits on one row.
        // cursor between the two chars (byte 3) → (0, 2).
        let s = "中中";
        assert_eq!(cursor_display_pos(s, 3, 4), (0, 2));
        // cursor at end (byte 6): no more content → stays at (0, 4).
        assert_eq!(cursor_display_pos(s, 6, 4), (0, 4));
    }

    #[test]
    fn cursor_wide_char_at_boundary_with_more_content() {
        // "中中中" width 4: rows ["中中", "中"].
        // cursor between 2nd and 3rd '中' (byte 6) → start of row 1.
        let s = "中中中";
        assert_eq!(cursor_display_pos(s, 6, 4), (1, 0));
    }

    #[test]
    fn cursor_at_exact_line_boundary_no_more_content() {
        // "abcde" exactly fills width 5, no following content.
        // Cursor at end → (0, 5), not (1, 0).
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
        // Buffer: "foo\bar" with cursor after '\' (byte 4).
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
        assert_eq!(buf, "hello"); // unchanged
    }

    #[test]
    fn backslash_enter_cursor_at_start_returns_none() {
        let mut buf = String::from("\\hello");
        let pos = apply_backslash_enter(&mut buf, 0);
        assert_eq!(pos, None);
        assert_eq!(buf, "\\hello"); // unchanged
    }

    #[test]
    fn backslash_enter_empty_buffer_returns_none() {
        let mut buf = String::new();
        let pos = apply_backslash_enter(&mut buf, 0);
        assert_eq!(pos, None);
    }

    #[test]
    fn backslash_enter_cursor_not_at_backslash_returns_none() {
        // Buffer "a\\b", cursor after 'a' (byte 1) — backslash is not before cursor.
        let mut buf = String::from("a\\b");
        let pos = apply_backslash_enter(&mut buf, 1);
        assert_eq!(pos, None);
        assert_eq!(buf, "a\\b"); // unchanged
    }

    #[test]
    fn backslash_enter_double_backslash_replaces_last_one() {
        // "foo\\\\" — two backslashes. Cursor at byte 5 (end).
        // Only the immediately preceding '\' is replaced.
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
        // "say @" — cursor right after '@', query is ""
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
        // Cursor is before the '@'
        assert!(find_at_context(input, 4).is_none());
    }

    #[test]
    fn find_at_context_uses_last_at() {
        let input = "@first @sec";
        let result = find_at_context(input, input.len());
        // Should match the second '@'
        assert_eq!(result, Some((7, "sec")));
    }

}

/// If the cursor is currently within a `@mention` context — i.e. there is an
/// `@` before `cursor_pos` with no space or newline between them — return the
/// byte index of the `@` and the query string typed after it.
fn find_at_context(input: &str, cursor_pos: usize) -> Option<(usize, &str)> {
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
fn apply_backslash_enter(buf: &mut String, cursor_pos: usize) -> Option<usize> {
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
/// Seed `online_users` from the broker's `/who` response content.
///
/// The broker sends `"online — alice, bob: away, charlie"` (or `"no users online"`).
/// Each entry is either a bare username or `username: status`; we extract the username part.
/// Merges into the existing list without removing users added by Join events.
fn seed_online_users_from_who(content: &str, online_users: &mut Vec<String>) {
    if let Some(rest) = content.strip_prefix("online — ") {
        for entry in rest.split(", ") {
            let username = entry.split(':').next().unwrap_or(entry).trim().to_owned();
            if !username.is_empty() && !online_users.contains(&username) {
                online_users.push(username);
            }
        }
    }
}

fn build_payload(input: &str) -> String {
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
