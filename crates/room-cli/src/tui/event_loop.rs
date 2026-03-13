use std::collections::HashMap;
use std::io;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    Terminal,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

use super::dm::{handle_dm_action, switch_to_tab, DmTabConfig};
use super::frame::{draw_frame, DrawContext};
use super::input::{
    build_payload, cursor_display_pos, handle_key, normalize_paste, wrap_input_display, Action,
    InputState,
};
use super::parse::parse_users_all_broadcast;
use super::render::{format_message, ColorMap, TabInfo};
use super::{DrainResult, RoomTab, MAX_INPUT_LINES};
use crate::message::Message;

pub async fn run(
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    write_half: tokio::net::unix::OwnedWriteHalf,
    room_id: &str,
    username: &str,
    history_lines: usize,
    socket_path: std::path::PathBuf,
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
        subscription_tiers: HashMap::new(),
        unread_count: 0,
        scroll_offset: 0,
        msg_rx,
        write_half,
    };

    // Redirect stderr to ~/.room/room.log so that eprintln! from the broker
    // (which runs in a background task) does not corrupt the TUI alternate screen.
    #[cfg(unix)]
    let saved_stderr_fd = redirect_stderr_to_log();

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
    let mut daemon_users: Vec<String> = Vec::new();
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

    // Seed daemon_users for cross-room @mention autocomplete.
    let who_all_payload = build_payload("/who_all");
    tabs[active_tab]
        .write_half
        .write_all(format!("{who_all_payload}\n").as_bytes())
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

        // Check for users_all: response from /who_all to populate cross-room users.
        for msg in tabs[active_tab].messages.iter().rev().take(5) {
            if let Message::System { user, content, .. } = msg {
                if user == "broker" {
                    if let Some(users) = parse_users_all_broadcast(content) {
                        daemon_users = users;
                        break;
                    }
                }
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
        let constraints: Vec<Constraint> = if show_tab_bar {
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

        let msg_texts: Vec<ratatui::text::Text<'static>> = tabs[active_tab]
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

        // Capture values needed by the draw call (avoid borrowing tabs inside closure).
        let scroll_offset = tabs[active_tab].scroll_offset;
        let room_id_display = tabs[active_tab].room_id.clone();
        let online_users_ref = &tabs[active_tab].online_users;
        let user_statuses_ref = &tabs[active_tab].user_statuses;
        let subscription_tiers_ref = &tabs[active_tab].subscription_tiers;
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

        let ctx = DrawContext {
            constraints: &constraints,
            show_tab_bar,
            tab_infos: &tab_infos,
            msg_texts: &msg_texts,
            heights: &heights,
            total_lines,
            scroll_offset,
            msg_area_height,
            room_id_display: &room_id_display,
            messages: messages_ref,
            online_users: online_users_ref,
            user_statuses: user_statuses_ref,
            subscription_tiers: subscription_tiers_ref,
            color_map: &color_map,
            input_state: &input_state,
            input_display_rows: &input_display_rows,
            visible_input_lines,
            total_input_rows,
            cursor_row,
            cursor_col,
            username,
            frame_count,
            splash_seed,
        };

        terminal.draw(|f| draw_frame(f, &ctx))?;

        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    let online_users = &tabs[active_tab].online_users;
                    match handle_key(
                        key,
                        &mut input_state,
                        online_users,
                        &daemon_users,
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
                                let next = (active_tab + 1) % tabs.len();
                                switch_to_tab(&mut tabs, &mut active_tab, &mut input_state, next);
                            }
                        }
                        Some(Action::PrevTab) => {
                            if tabs.len() > 1 {
                                let prev = if active_tab == 0 {
                                    tabs.len() - 1
                                } else {
                                    active_tab - 1
                                };
                                switch_to_tab(&mut tabs, &mut active_tab, &mut input_state, prev);
                            }
                        }
                        Some(Action::SwitchTab(idx)) => {
                            if idx < tabs.len() {
                                switch_to_tab(&mut tabs, &mut active_tab, &mut input_state, idx);
                            }
                        }
                        Some(Action::DmRoom {
                            target_user,
                            content,
                        }) => {
                            let cfg = DmTabConfig {
                                socket_path: &socket_path,
                                username,
                                history_lines,
                            };
                            if let Err(e) = handle_dm_action(
                                &mut tabs,
                                &mut active_tab,
                                &mut input_state,
                                &cfg,
                                target_user,
                                content,
                            )
                            .await
                            {
                                result = Err(e);
                                break 'main;
                            }
                        }
                        None => {}
                    }
                }
                Event::Paste(text) => {
                    let clean = normalize_paste(&text);
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

    // Restore stderr so post-TUI error messages appear on the terminal.
    #[cfg(unix)]
    restore_stderr(saved_stderr_fd);

    result
}

// ── Stderr redirection ───────────────────────────────────────────────────────

/// Redirect stderr (fd 2) to `~/.room/room.log` so that `eprintln!` output
/// from the broker does not corrupt the TUI alternate screen. Returns the
/// saved fd so it can be restored after the TUI exits.
#[cfg(unix)]
fn redirect_stderr_to_log() -> Option<i32> {
    let log_path = crate::paths::room_home().join("room.log");

    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(_) => return None,
    };

    // Save the current stderr fd so we can restore it later.
    let saved = unsafe { libc::dup(libc::STDERR_FILENO) };
    if saved < 0 {
        return None;
    }

    let log_fd = file.as_raw_fd();
    if unsafe { libc::dup2(log_fd, libc::STDERR_FILENO) } < 0 {
        unsafe { libc::close(saved) };
        return None;
    }

    Some(saved)
}

/// Restore stderr to its original fd after leaving the TUI.
#[cfg(unix)]
fn restore_stderr(saved: Option<i32>) {
    if let Some(fd) = saved {
        unsafe {
            libc::dup2(fd, libc::STDERR_FILENO);
            libc::close(fd);
        }
    }
}
