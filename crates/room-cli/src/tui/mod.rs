use std::collections::HashMap;
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
    build_payload, cursor_display_pos, handle_key, parse_kick_broadcast, parse_status_broadcast,
    seed_online_users_from_who, wrap_input_display, Action, InputState,
};
use render::{
    assign_color, find_view_start, format_message, render_tab_bar, user_color, welcome_splash,
    ColorMap, TabInfo,
};

/// Maximum visible content lines in the input box before it stops growing.
const MAX_INPUT_LINES: usize = 6;

/// Per-room state for the tabbed TUI. Each tab owns its message buffer,
/// online user list, status map, and inbound message channel.
struct RoomTab {
    room_id: String,
    messages: Vec<Message>,
    online_users: Vec<String>,
    user_statuses: HashMap<String, String>,
    unread_count: usize,
    scroll_offset: usize,
    msg_rx: mpsc::UnboundedReceiver<Message>,
    write_half: tokio::net::unix::OwnedWriteHalf,
}

/// Result of draining messages from a tab's channel.
enum DrainResult {
    /// Channel still open, messages drained.
    Ok,
    /// Channel closed — broker disconnected.
    Disconnected,
}

impl RoomTab {
    /// Process a single inbound message, updating online_users, statuses, and
    /// the color map. Pushes the message into the buffer and increments unread
    /// if `is_active` is false.
    fn process_message(&mut self, msg: Message, color_map: &mut ColorMap, is_active: bool) {
        match &msg {
            Message::Join { user, .. } if !self.online_users.contains(user) => {
                assign_color(user, color_map);
                self.online_users.push(user.clone());
            }
            Message::Leave { user, .. } => {
                self.online_users.retain(|u| u != user);
                self.user_statuses.remove(user);
            }
            Message::Message { user, .. } if !self.online_users.contains(user) => {
                assign_color(user, color_map);
                self.online_users.push(user.clone());
            }
            Message::Message { user, .. } => {
                assign_color(user, color_map);
            }
            Message::System { user, content, .. } if user == "broker" => {
                seed_online_users_from_who(
                    content,
                    &mut self.online_users,
                    &mut self.user_statuses,
                );
                if let Some((name, status)) = parse_status_broadcast(content) {
                    self.user_statuses.insert(name, status);
                }
                if let Some(kicked) = parse_kick_broadcast(content) {
                    self.online_users.retain(|u| u != kicked);
                    self.user_statuses.remove(kicked);
                }
                for u in &self.online_users {
                    assign_color(u, color_map);
                }
            }
            _ => {}
        }
        if !is_active {
            self.unread_count += 1;
        }
        self.messages.push(msg);
    }

    /// Drain all pending messages from the channel into the message buffer.
    fn drain_messages(&mut self, color_map: &mut ColorMap, is_active: bool) -> DrainResult {
        loop {
            match self.msg_rx.try_recv() {
                Ok(msg) => self.process_message(msg, color_map, is_active),
                Err(mpsc::error::TryRecvError::Empty) => return DrainResult::Ok,
                Err(mpsc::error::TryRecvError::Disconnected) => return DrainResult::Disconnected,
            }
        }
    }
}

pub async fn run(
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    write_half: tokio::net::unix::OwnedWriteHalf,
    room_id: &str,
    username: &str,
    history_lines: usize,
) -> anyhow::Result<()> {
    let (msg_tx, msg_rx) = mpsc::unbounded_channel::<Message>();
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

    let tab = RoomTab {
        room_id: room_id.to_owned(),
        messages: Vec::new(),
        online_users: Vec::new(),
        user_statuses: HashMap::new(),
        unread_count: 0,
        scroll_offset: 0,
        msg_rx,
        write_half,
    };

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut tabs: Vec<RoomTab> = vec![tab];
    let mut active_tab: usize = 0;
    let mut color_map = ColorMap::new();
    let mut input_state = InputState::new();
    let mut result: anyhow::Result<()> = Ok(());
    let mut frame_count: usize = 0;

    // Seed for generative bot faces — fixed per session so the splash is stable.
    let splash_seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            d.as_secs()
                .wrapping_mul(6364136223846793005)
                .wrapping_add(d.subsec_nanos() as u64)
        })
        .unwrap_or(0xdeadbeef_cafebabe);

    // Seed online_users immediately so @mention autocomplete works for users
    // who were already connected before we joined.
    let who_payload = build_payload("/who");
    tabs[active_tab]
        .write_half
        .write_all(format!("{who_payload}\n").as_bytes())
        .await?;

    'main: loop {
        // Sync scroll_offset: handle_key modifies input_state.scroll_offset,
        // but rendering reads from tabs[active_tab].scroll_offset.
        tabs[active_tab].scroll_offset = input_state.scroll_offset;

        // Drain pending messages from all tabs.
        for (i, t) in tabs.iter_mut().enumerate() {
            let is_active = i == active_tab;
            if matches!(
                t.drain_messages(&mut color_map, is_active),
                DrainResult::Disconnected
            ) && is_active
            {
                break 'main;
            }
        }

        let show_tab_bar = tabs.len() > 1;

        let term_area = terminal.size()?;
        // Input content width is terminal width minus the two border columns.
        let input_content_width = term_area.width.saturating_sub(2) as usize;

        // Compute wrapped display rows for the input and the cursor position within them.
        let input_display_rows = wrap_input_display(&input_state.input, input_content_width);
        let total_input_rows = input_display_rows.len();
        let visible_input_lines = total_input_rows.min(MAX_INPUT_LINES);
        // +2 for top and bottom borders; minimum 3 (1 content line + 2 borders).
        let input_box_height = (visible_input_lines + 2) as u16;

        let (cursor_row, cursor_col) = cursor_display_pos(
            &input_state.input,
            input_state.cursor_pos,
            input_content_width,
        );

        // Adjust vertical scroll so the cursor stays visible.
        if cursor_row < input_state.input_row_scroll {
            input_state.input_row_scroll = cursor_row;
        }
        if visible_input_lines > 0
            && cursor_row >= input_state.input_row_scroll + visible_input_lines
        {
            input_state.input_row_scroll = cursor_row + 1 - visible_input_lines;
        }

        let content_width = term_area.width.saturating_sub(2) as usize;

        // Build layout constraints: optional tab bar + message area + input box.
        let constraints = if show_tab_bar {
            vec![
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(input_box_height),
            ]
        } else {
            vec![Constraint::Min(3), Constraint::Length(input_box_height)]
        };

        // Compute visible message lines by pre-computing the layout split.
        let msg_area_height = {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints.clone())
                .split(Rect::new(0, 0, term_area.width, term_area.height));
            let msg_chunk = if show_tab_bar { chunks[1] } else { chunks[0] };
            msg_chunk.height.saturating_sub(2) as usize
        };

        let msg_texts: Vec<Text<'static>> = tabs[active_tab]
            .messages
            .iter()
            .map(|m| format_message(m, content_width, &color_map))
            .collect();

        let heights: Vec<usize> = msg_texts.iter().map(|t| t.lines.len().max(1)).collect();
        let total_lines: usize = heights.iter().sum();

        // Clamp scroll offset so it can't exceed scrollable range
        tabs[active_tab].scroll_offset = tabs[active_tab]
            .scroll_offset
            .min(total_lines.saturating_sub(msg_area_height));
        // Sync clamped value back to input_state so handle_key sees the clamped value.
        input_state.scroll_offset = tabs[active_tab].scroll_offset;

        // Capture values needed by the draw closure (avoid borrowing tabs inside closure).
        let scroll_offset = tabs[active_tab].scroll_offset;
        let room_id_display = tabs[active_tab].room_id.clone();
        let online_users_ref = &tabs[active_tab].online_users;
        let user_statuses_ref = &tabs[active_tab].user_statuses;
        let messages_ref = &tabs[active_tab].messages;

        // Build tab bar info for multi-tab rendering.
        let tab_infos: Vec<TabInfo> = tabs
            .iter()
            .enumerate()
            .map(|(i, t)| TabInfo {
                room_id: t.room_id.clone(),
                active: i == active_tab,
                unread: t.unread_count,
            })
            .collect();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints.clone())
                .split(f.area());

            let (tab_bar_chunk, msg_chunk, input_chunk) = if show_tab_bar {
                (Some(chunks[0]), chunks[1], chunks[2])
            } else {
                (None, chunks[0], chunks[1])
            };

            // Render tab bar if multi-tab.
            if let Some(bar_area) = tab_bar_chunk {
                if let Some(bar_line) = render_tab_bar(&tab_infos) {
                    let bar_widget =
                        Paragraph::new(bar_line).style(Style::default().bg(Color::Black));
                    f.render_widget(bar_widget, bar_area);
                }
            }

            let view_bottom = total_lines.saturating_sub(scroll_offset);
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

            let title = if scroll_offset > 0 {
                format!(" {} [↑ {} lines] ", room_id_display, scroll_offset)
            } else {
                format!(" {} ", room_id_display)
            };

            // Show the welcome splash when there are no chat messages yet.
            let has_chat = messages_ref.iter().any(|m| {
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
                let splash_width = msg_chunk.width.saturating_sub(2) as usize;
                let splash = welcome_splash(frame_count, splash_width, splash_seed);
                let splash_widget = Paragraph::new(splash)
                    .block(
                        Block::default()
                            .title(title.clone())
                            .title_top(version_title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    )
                    .alignment(Alignment::Left);
                f.render_widget(splash_widget, msg_chunk);
            } else {
                let msg_list = List::new(visible).block(
                    Block::default()
                        .title(title)
                        .title_top(version_title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
                f.render_widget(msg_list, msg_chunk);
            }

            // Render only the visible slice of wrapped input rows.
            let end = (input_state.input_row_scroll + visible_input_lines).min(total_input_rows);
            let display_text = input_display_rows[input_state.input_row_scroll..end].join("\n");

            let input_widget = Paragraph::new(display_text)
                .block(
                    Block::default()
                        .title(format!(" {username} "))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                )
                .style(Style::default().fg(Color::White));
            f.render_widget(input_widget, input_chunk);

            // Place terminal cursor inside the input box.
            let visible_cursor_row = cursor_row - input_state.input_row_scroll;
            let cursor_x = input_chunk.x + 1 + cursor_col as u16;
            let cursor_y = input_chunk.y + 1 + visible_cursor_row as u16;
            f.set_cursor_position((cursor_x, cursor_y));

            // Render floating member status panel (top-right of message area).
            // Hidden when terminal is too narrow (< 80 cols) or no users online.
            const PANEL_MIN_TERM_WIDTH: u16 = 80;
            if f.area().width >= PANEL_MIN_TERM_WIDTH && !online_users_ref.is_empty() {
                let panel_items: Vec<ListItem> = online_users_ref
                    .iter()
                    .map(|u| {
                        let status = user_statuses_ref.get(u).map(|s| s.as_str()).unwrap_or("");
                        let mut spans = vec![Span::styled(
                            format!(" {u}"),
                            Style::default()
                                .fg(user_color(u, &color_map))
                                .add_modifier(Modifier::BOLD),
                        )];
                        if !status.is_empty() {
                            spans.push(Span::styled(
                                format!("  {status}"),
                                Style::default().fg(Color::DarkGray),
                            ));
                        }
                        spans.push(Span::raw(" "));
                        ListItem::new(Line::from(spans))
                    })
                    .collect();

                let panel_content_width = online_users_ref
                    .iter()
                    .map(|u| {
                        let status = user_statuses_ref.get(u).map(|s| s.as_str()).unwrap_or("");
                        let status_len = if status.is_empty() {
                            0
                        } else {
                            status.len() + 2 // "  " + status
                        };
                        u.len() + 1 + status_len + 1 // " " + name + status_part + " "
                    })
                    .max()
                    .unwrap_or(10);
                let panel_width = (panel_content_width as u16 + 2)
                    .min(msg_chunk.width / 3)
                    .max(12);
                let panel_height =
                    (online_users_ref.len() as u16 + 2).min(msg_chunk.height.saturating_sub(1));

                let panel_x = msg_chunk.x + msg_chunk.width - panel_width - 1;
                let panel_y = msg_chunk.y + 1;

                let panel_rect = Rect {
                    x: panel_x,
                    y: panel_y,
                    width: panel_width,
                    height: panel_height,
                };

                f.render_widget(Clear, panel_rect);
                let panel = List::new(panel_items).block(
                    Block::default()
                        .title(" members ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
                f.render_widget(panel, panel_rect);
            }

            // Render the command palette popup above the input box when active.
            if input_state.palette.active && !input_state.palette.filtered.is_empty() {
                let palette_items: Vec<ListItem> = input_state
                    .palette
                    .filtered
                    .iter()
                    .enumerate()
                    .map(|(row, &idx)| {
                        let item = &input_state.palette.commands[idx];
                        let style = if row == input_state.palette.selected {
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
                                if row == input_state.palette.selected {
                                    Style::default().fg(Color::Black).bg(Color::Cyan)
                                } else {
                                    Style::default().fg(Color::DarkGray)
                                },
                            ),
                        ]))
                    })
                    .collect();

                let popup_height =
                    (input_state.palette.filtered.len() as u16 + 2).min(msg_chunk.height);
                let popup_y = input_chunk.y.saturating_sub(popup_height);
                let popup_rect = Rect {
                    x: input_chunk.x,
                    y: popup_y,
                    width: input_chunk.width,
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
            if input_state.mention.active && !input_state.mention.filtered.is_empty() {
                let mention_items: Vec<ListItem> = input_state
                    .mention
                    .filtered
                    .iter()
                    .enumerate()
                    .map(|(row, user)| {
                        let style = if row == input_state.mention.selected {
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

                let popup_height =
                    (input_state.mention.filtered.len() as u16 + 2).min(msg_chunk.height);
                let popup_y = input_chunk.y.saturating_sub(popup_height);
                let max_width = input_state
                    .mention
                    .filtered
                    .iter()
                    .map(|u| u.len() + 1) // '@' + username
                    .max()
                    .unwrap_or(8) as u16
                    + 4; // borders + padding
                let popup_width = max_width.min(input_chunk.width / 2).max(8);
                let popup_x = cursor_x
                    .saturating_sub(1)
                    .min(input_chunk.x + input_chunk.width.saturating_sub(popup_width));
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
                    let online_users = &tabs[active_tab].online_users;
                    match handle_key(
                        key,
                        &mut input_state,
                        online_users,
                        msg_area_height,
                        input_content_width,
                    ) {
                        Some(Action::Send(payload)) => {
                            if let Err(e) = tabs[active_tab]
                                .write_half
                                .write_all(format!("{payload}\n").as_bytes())
                                .await
                            {
                                result = Err(e.into());
                                break 'main;
                            }
                        }
                        Some(Action::Quit) => break 'main,
                        Some(Action::NextTab) => {
                            if tabs.len() > 1 {
                                tabs[active_tab].scroll_offset = input_state.scroll_offset;
                                active_tab = (active_tab + 1) % tabs.len();
                                tabs[active_tab].unread_count = 0;
                                input_state.scroll_offset = tabs[active_tab].scroll_offset;
                            }
                        }
                        Some(Action::PrevTab) => {
                            if tabs.len() > 1 {
                                tabs[active_tab].scroll_offset = input_state.scroll_offset;
                                active_tab = if active_tab == 0 {
                                    tabs.len() - 1
                                } else {
                                    active_tab - 1
                                };
                                tabs[active_tab].unread_count = 0;
                                input_state.scroll_offset = tabs[active_tab].scroll_offset;
                            }
                        }
                        Some(Action::SwitchTab(idx)) => {
                            if idx < tabs.len() {
                                tabs[active_tab].scroll_offset = input_state.scroll_offset;
                                active_tab = idx;
                                tabs[active_tab].unread_count = 0;
                                input_state.scroll_offset = tabs[active_tab].scroll_offset;
                            }
                        }
                        None => {}
                    }
                }
                Event::Paste(text) => {
                    // Normalize line endings: \r\n → \n, stray \r → \n.
                    let clean = text.replace("\r\n", "\n").replace('\r', "\n");
                    input_state.input.insert_str(input_state.cursor_pos, &clean);
                    input_state.cursor_pos += clean.len();
                    input_state.mention.active = false;
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        // Drain any messages that arrived during the poll (all tabs).
        for (i, t) in tabs.iter_mut().enumerate() {
            let is_active = i == active_tab;
            if matches!(
                t.drain_messages(&mut color_map, is_active),
                DrainResult::Disconnected
            ) && is_active
            {
                break 'main;
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_msg(user: &str, content: &str) -> Message {
        Message::Message {
            id: "test-id".into(),
            room: "test-room".into(),
            user: user.into(),
            ts: Utc::now(),
            content: content.into(),
            seq: None,
        }
    }

    fn make_join(user: &str) -> Message {
        Message::Join {
            id: "test-id".into(),
            room: "test-room".into(),
            user: user.into(),
            ts: Utc::now(),
            seq: None,
        }
    }

    fn make_leave(user: &str) -> Message {
        Message::Leave {
            id: "test-id".into(),
            room: "test-room".into(),
            user: user.into(),
            ts: Utc::now(),
            seq: None,
        }
    }

    fn make_system(content: &str) -> Message {
        Message::System {
            id: "test-id".into(),
            room: "test-room".into(),
            user: "broker".into(),
            ts: Utc::now(),
            content: content.into(),
            seq: None,
        }
    }

    // ── RoomTab::process_message tests ────────────────────────────────────

    #[tokio::test]
    async fn process_message_adds_user_on_join() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_join("alice"), &mut cm, true);
        assert_eq!(tab.online_users, vec!["alice"]);
        assert_eq!(tab.messages.len(), 1);
    }

    #[tokio::test]
    async fn process_message_removes_user_on_leave() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: vec!["alice".into()],
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_leave("alice"), &mut cm, true);
        assert!(tab.online_users.is_empty());
    }

    #[tokio::test]
    async fn process_message_increments_unread_when_inactive() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_msg("bob", "hello"), &mut cm, false);
        assert_eq!(tab.unread_count, 1);

        tab.process_message(make_msg("bob", "world"), &mut cm, false);
        assert_eq!(tab.unread_count, 2);
    }

    #[tokio::test]
    async fn process_message_no_unread_when_active() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_msg("bob", "hello"), &mut cm, true);
        assert_eq!(tab.unread_count, 0);
    }

    #[tokio::test]
    async fn process_message_seeds_user_from_message_sender() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_msg("charlie", "hi"), &mut cm, true);
        assert_eq!(tab.online_users, vec!["charlie"]);
        assert!(cm.contains_key("charlie"));
    }

    #[tokio::test]
    async fn process_message_does_not_duplicate_existing_user() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: vec!["alice".into()],
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_msg("alice", "hi"), &mut cm, true);
        assert_eq!(tab.online_users.len(), 1);
    }

    // ── drain_messages tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn drain_messages_processes_pending() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tx.send(make_msg("bob", "one")).unwrap();
        tx.send(make_msg("bob", "two")).unwrap();

        let result = tab.drain_messages(&mut cm, true);
        assert!(matches!(result, DrainResult::Ok));
        assert_eq!(tab.messages.len(), 2);
    }

    #[tokio::test]
    async fn drain_messages_detects_disconnect() {
        let (tx, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        drop(tx);
        let result = tab.drain_messages(&mut cm, true);
        assert!(matches!(result, DrainResult::Disconnected));
    }

    #[tokio::test]
    async fn drain_messages_empty_returns_ok() {
        let (_tx, rx) = mpsc::unbounded_channel::<Message>();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: Vec::new(),
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        let result = tab.drain_messages(&mut cm, true);
        assert!(matches!(result, DrainResult::Ok));
        assert!(tab.messages.is_empty());
    }

    #[tokio::test]
    async fn process_system_message_parses_status() {
        let (_, rx) = mpsc::unbounded_channel();
        let (_, wh) = tokio::net::UnixStream::pair().unwrap().1.into_split();
        let mut tab = RoomTab {
            room_id: "test".into(),
            messages: Vec::new(),
            online_users: vec!["alice".into()],
            user_statuses: HashMap::new(),
            unread_count: 0,
            scroll_offset: 0,
            msg_rx: rx,
            write_half: wh,
        };
        let mut cm = ColorMap::new();

        tab.process_message(make_system("alice set status: coding"), &mut cm, true);
        assert_eq!(tab.user_statuses.get("alice").unwrap(), "coding");
    }
}
