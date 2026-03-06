use std::io;

use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Terminal,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

mod input;
mod render;
mod widgets;

use crate::message::Message;
use input::{
    build_payload, cursor_display_pos, handle_key, seed_online_users_from_who, wrap_input_display,
    Action, InputState,
};
use render::{assign_color, find_view_start, format_message, user_color, welcome_splash, ColorMap};

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
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut messages: Vec<Message> = Vec::new();
    let mut online_users: Vec<String> = Vec::new();
    let mut color_map = ColorMap::new();
    let mut state = InputState::new();
    let mut result: anyhow::Result<()> = Ok(());
    let mut frame_count: usize = 0;

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
                            assign_color(user, &mut color_map);
                            online_users.push(user.clone());
                        }
                        Message::Leave { user, .. } => {
                            online_users.retain(|u| u != user);
                        }
                        // Seed from message senders so @mention works for users who connected
                        // via poll/send (no persistent connection, not in the status_map).
                        Message::Message { user, .. } if !online_users.contains(user) => {
                            assign_color(user, &mut color_map);
                            online_users.push(user.clone());
                        }
                        // Assign color for any message sender not yet in the map.
                        Message::Message { user, .. } => {
                            assign_color(user, &mut color_map);
                        }
                        // Parse the /who response to seed the authoritative user list.
                        Message::System { user, content, .. } if user == "broker" => {
                            seed_online_users_from_who(content, &mut online_users);
                            for u in &online_users {
                                assign_color(u, &mut color_map);
                            }
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
        let input_display_rows = wrap_input_display(&state.input, input_content_width);
        let total_input_rows = input_display_rows.len();
        let visible_input_lines = total_input_rows.min(MAX_INPUT_LINES);
        // +2 for top and bottom borders; minimum 3 (1 content line + 2 borders).
        let input_box_height = (visible_input_lines + 2) as u16;

        let (cursor_row, cursor_col) =
            cursor_display_pos(&state.input, state.cursor_pos, input_content_width);

        // Adjust vertical scroll so the cursor stays visible.
        if cursor_row < state.input_row_scroll {
            state.input_row_scroll = cursor_row;
        }
        if visible_input_lines > 0 && cursor_row >= state.input_row_scroll + visible_input_lines {
            state.input_row_scroll = cursor_row + 1 - visible_input_lines;
        }

        let content_width = term_area.width.saturating_sub(2) as usize;
        // Compute visible message lines by pre-computing the layout split.
        // This must match the Layout used in the draw closure so scroll
        // clamping and viewport computation use the same height.
        let msg_area_height = {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(input_box_height)])
                .split(Rect::new(0, 0, term_area.width, term_area.height));
            chunks[0].height.saturating_sub(2) as usize
        };

        let msg_texts: Vec<Text<'static>> = messages
            .iter()
            .map(|m| format_message(m, content_width, &color_map))
            .collect();

        let heights: Vec<usize> = msg_texts.iter().map(|t| t.lines.len().max(1)).collect();
        let total_lines: usize = heights.iter().sum();

        // Clamp scroll offset so it can't exceed scrollable range
        state.scroll_offset = state
            .scroll_offset
            .min(total_lines.saturating_sub(msg_area_height));

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(input_box_height)])
                .split(f.area());

            let view_bottom = total_lines.saturating_sub(state.scroll_offset);
            let view_top = view_bottom.saturating_sub(msg_area_height);

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

            let title = if state.scroll_offset > 0 {
                format!(" {room_id} [↑ {} lines] ", state.scroll_offset)
            } else {
                format!(" {room_id} ")
            };

            // Show the welcome splash when there are no chat messages yet.
            let has_chat = messages.iter().any(|m| {
                matches!(
                    m,
                    Message::Message { .. }
                        | Message::Reply { .. }
                        | Message::Command { .. }
                        | Message::DirectMessage { .. }
                )
            });

            let version_title =
                Line::from(format!(" v{} ", env!("CARGO_PKG_VERSION"))).alignment(Alignment::Right);

            if !has_chat {
                let splash_width = chunks[0].width.saturating_sub(2) as usize;
                let splash = welcome_splash(frame_count, splash_width);
                let splash_widget = Paragraph::new(splash)
                    .block(
                        Block::default()
                            .title(title.clone())
                            .title_top(version_title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    )
                    .alignment(Alignment::Left);
                f.render_widget(splash_widget, chunks[0]);
            } else {
                let msg_list = List::new(visible).block(
                    Block::default()
                        .title(title)
                        .title_top(version_title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
                f.render_widget(msg_list, chunks[0]);
            }

            // Render only the visible slice of wrapped input rows.
            let end = (state.input_row_scroll + visible_input_lines).min(total_input_rows);
            let display_text = input_display_rows[state.input_row_scroll..end].join("\n");

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
            let visible_cursor_row = cursor_row - state.input_row_scroll;
            let cursor_x = chunks[1].x + 1 + cursor_col as u16;
            let cursor_y = chunks[1].y + 1 + visible_cursor_row as u16;
            f.set_cursor_position((cursor_x, cursor_y));

            // Render the command palette popup above the input box when active.
            if state.palette.active && !state.palette.filtered.is_empty() {
                let palette_items: Vec<ListItem> = state
                    .palette
                    .filtered
                    .iter()
                    .enumerate()
                    .map(|(row, &idx)| {
                        let item = &state.palette.commands[idx];
                        let style = if row == state.palette.selected {
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
                                if row == state.palette.selected {
                                    Style::default().fg(Color::Black).bg(Color::Cyan)
                                } else {
                                    Style::default().fg(Color::DarkGray)
                                },
                            ),
                        ]))
                    })
                    .collect();

                let popup_height = (state.palette.filtered.len() as u16 + 2).min(chunks[0].height);
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
            if state.mention.active && !state.mention.filtered.is_empty() {
                let mention_items: Vec<ListItem> = state
                    .mention
                    .filtered
                    .iter()
                    .enumerate()
                    .map(|(row, user)| {
                        let style = if row == state.mention.selected {
                            Style::default()
                                .fg(Color::Black)
                                .bg(user_color(user, &color_map))
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(user_color(user, &color_map))
                        };
                        ListItem::new(Line::from(Span::styled(format!("@{user}"), style)))
                    })
                    .collect();

                let popup_height = (state.mention.filtered.len() as u16 + 2).min(chunks[0].height);
                let popup_y = chunks[1].y.saturating_sub(popup_height);
                let max_width = state
                    .mention
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
                Event::Key(key) => {
                    match handle_key(
                        key,
                        &mut state,
                        &online_users,
                        msg_area_height,
                        input_content_width,
                    ) {
                        Some(Action::Send(payload)) => {
                            if let Err(e) = write_half
                                .write_all(format!("{payload}\n").as_bytes())
                                .await
                            {
                                result = Err(e.into());
                                break 'main;
                            }
                        }
                        Some(Action::Quit) => break 'main,
                        None => {}
                    }
                }
                Event::Paste(text) => {
                    // Normalize line endings: \r\n → \n, stray \r → \n.
                    let clean = text.replace("\r\n", "\n").replace('\r', "\n");
                    state.input.insert_str(state.cursor_pos, &clean);
                    state.cursor_pos += clean.len();
                    state.mention.active = false;
                }
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
                            assign_color(user, &mut color_map);
                            online_users.push(user.clone());
                        }
                        Message::Leave { user, .. } => {
                            online_users.retain(|u| u != user);
                        }
                        Message::Message { user, .. } if !online_users.contains(user) => {
                            assign_color(user, &mut color_map);
                            online_users.push(user.clone());
                        }
                        Message::Message { user, .. } => {
                            assign_color(user, &mut color_map);
                        }
                        Message::System { user, content, .. } if user == "broker" => {
                            seed_online_users_from_who(content, &mut online_users);
                            for u in &online_users {
                                assign_color(u, &mut color_map);
                            }
                        }
                        _ => {}
                    }
                    messages.push(msg);
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break 'main,
            }
        }

        frame_count = frame_count.wrapping_add(1);
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result
}
