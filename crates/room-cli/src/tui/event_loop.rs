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

// ── Extracted helpers ────────────────────────────────────────────────────────

/// Spawn a background task that reads from the broker socket, buffers history
/// until the user's own join event, then streams live messages through the
/// returned channel.
fn setup_socket_reader(
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    username: String,
    history_lines: usize,
) -> mpsc::UnboundedReceiver<Message> {
    let (msg_tx, msg_rx) = mpsc::unbounded_channel::<Message>();

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
                            matches!(&msg, Message::Join { user, .. } if user == &username);
                        if is_own_join {
                            joined = true;
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

    msg_rx
}

/// Pre-computed layout dimensions for a single frame.
///
/// All fields are derived from the terminal size and input state. This struct
/// is cheap to create and makes the layout computation unit-testable.
pub(super) struct LayoutMetrics {
    pub(super) show_tab_bar: bool,
    pub(super) constraints: Vec<Constraint>,
    pub(super) input_content_width: usize,
    pub(super) content_width: usize,
    pub(super) msg_area_height: usize,
    pub(super) input_display_rows: Vec<String>,
    pub(super) visible_input_lines: usize,
    pub(super) total_input_rows: usize,
    pub(super) cursor_row: usize,
    pub(super) cursor_col: usize,
}

/// Compute layout dimensions from the terminal area and current input state.
///
/// This is a pure function (aside from adjusting `input_state.input_row_scroll`
/// to keep the cursor visible). The returned [`LayoutMetrics`] contains
/// everything needed to build a [`DrawContext`] and handle events.
pub(super) fn compute_layout_metrics(
    term_area: Rect,
    input_state: &mut InputState,
    tab_count: usize,
) -> LayoutMetrics {
    let show_tab_bar = tab_count > 1;

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
    if visible_input_lines > 0 && cursor_row >= input_state.input_row_scroll + visible_input_lines {
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

    LayoutMetrics {
        show_tab_bar,
        constraints,
        input_content_width,
        content_width,
        msg_area_height,
        input_display_rows,
        visible_input_lines,
        total_input_rows,
        cursor_row,
        cursor_col,
    }
}

/// Outcome of processing a single terminal event.
enum EventAction {
    /// Continue the main loop.
    Continue,
    /// Exit the main loop (quit or disconnect).
    Break,
    /// Exit the main loop with an error.
    Error(anyhow::Error),
}

/// Immutable per-session parameters passed to [`handle_event`].
struct EventConfig<'a> {
    daemon_users: &'a [String],
    socket_path: &'a std::path::Path,
    username: &'a str,
    history_lines: usize,
}

/// Poll for a terminal event and dispatch it, returning the loop action.
///
/// This handles key presses (delegating to [`handle_key`]), paste events, and
/// resize events. Tab switching and DM actions that require async I/O are
/// handled inline.
async fn handle_event(
    tabs: &mut Vec<RoomTab>,
    active_tab: &mut usize,
    input_state: &mut InputState,
    msg_area_height: usize,
    input_content_width: usize,
    cfg: &EventConfig<'_>,
) -> std::io::Result<EventAction> {
    if !event::poll(std::time::Duration::from_millis(50))? {
        return Ok(EventAction::Continue);
    }

    match event::read()? {
        Event::Key(key) => {
            let online_users = &tabs[*active_tab].online_users;
            match handle_key(
                key,
                input_state,
                online_users,
                cfg.daemon_users,
                msg_area_height,
                input_content_width,
            ) {
                Some(Action::Send(payload)) => {
                    if let Err(e) = tabs[*active_tab]
                        .write_half
                        .write_all(format!("{payload}\n").as_bytes())
                        .await
                    {
                        return Ok(EventAction::Error(e.into()));
                    }
                }
                Some(Action::Quit) => return Ok(EventAction::Break),
                Some(Action::NextTab) => {
                    if tabs.len() > 1 {
                        let next = (*active_tab + 1) % tabs.len();
                        switch_to_tab(tabs, active_tab, input_state, next);
                    }
                }
                Some(Action::PrevTab) => {
                    if tabs.len() > 1 {
                        let prev = if *active_tab == 0 {
                            tabs.len() - 1
                        } else {
                            *active_tab - 1
                        };
                        switch_to_tab(tabs, active_tab, input_state, prev);
                    }
                }
                Some(Action::SwitchTab(idx)) => {
                    if idx < tabs.len() {
                        switch_to_tab(tabs, active_tab, input_state, idx);
                    }
                }
                Some(Action::DmRoom {
                    target_user,
                    content,
                }) => {
                    let dm_cfg = DmTabConfig {
                        socket_path: cfg.socket_path,
                        username: cfg.username,
                        history_lines: cfg.history_lines,
                    };
                    if let Err(e) = handle_dm_action(
                        tabs,
                        active_tab,
                        input_state,
                        &dm_cfg,
                        target_user,
                        content,
                    )
                    .await
                    {
                        return Ok(EventAction::Error(e));
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

    Ok(EventAction::Continue)
}

// ── Main entry point ─────────────────────────────────────────────────────────

pub async fn run(
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    write_half: tokio::net::unix::OwnedWriteHalf,
    room_id: &str,
    username: &str,
    history_lines: usize,
    socket_path: std::path::PathBuf,
) -> anyhow::Result<()> {
    let msg_rx = setup_socket_reader(reader, username.to_owned(), history_lines);

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

    #[cfg(unix)]
    let saved_stderr_fd = redirect_stderr_to_log();

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

        let metrics = compute_layout_metrics(terminal.size()?.into(), &mut input_state, tabs.len());

        // Format messages and compute scroll bounds.
        let msg_texts: Vec<ratatui::text::Text<'static>> = tabs[active_tab]
            .messages
            .iter()
            .map(|m| format_message(m, metrics.content_width, &color_map))
            .collect();
        let heights: Vec<usize> = msg_texts.iter().map(|t| t.lines.len().max(1)).collect();
        let total_lines: usize = heights.iter().sum();

        // Clamp scroll offset so it can't exceed scrollable range.
        tabs[active_tab].scroll_offset = tabs[active_tab]
            .scroll_offset
            .min(total_lines.saturating_sub(metrics.msg_area_height));
        input_state.scroll_offset = tabs[active_tab].scroll_offset;

        let scroll_offset = tabs[active_tab].scroll_offset;
        let room_id_display = tabs[active_tab].room_id.clone();
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
            constraints: &metrics.constraints,
            show_tab_bar: metrics.show_tab_bar,
            tab_infos: &tab_infos,
            msg_texts: &msg_texts,
            heights: &heights,
            total_lines,
            scroll_offset,
            msg_area_height: metrics.msg_area_height,
            room_id_display: &room_id_display,
            messages: &tabs[active_tab].messages,
            online_users: &tabs[active_tab].online_users,
            user_statuses: &tabs[active_tab].user_statuses,
            subscription_tiers: &tabs[active_tab].subscription_tiers,
            color_map: &color_map,
            input_state: &input_state,
            input_display_rows: &metrics.input_display_rows,
            visible_input_lines: metrics.visible_input_lines,
            total_input_rows: metrics.total_input_rows,
            cursor_row: metrics.cursor_row,
            cursor_col: metrics.cursor_col,
            username,
            frame_count,
            splash_seed,
        };

        terminal.draw(|f| draw_frame(f, &ctx))?;

        let event_cfg = EventConfig {
            daemon_users: &daemon_users,
            socket_path: &socket_path,
            username,
            history_lines,
        };
        match handle_event(
            &mut tabs,
            &mut active_tab,
            &mut input_state,
            metrics.msg_area_height,
            metrics.input_content_width,
            &event_cfg,
        )
        .await?
        {
            EventAction::Continue => {}
            EventAction::Break => break 'main,
            EventAction::Error(e) => {
                result = Err(e);
                break 'main;
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn layout_metrics_single_tab() {
        let mut input_state = InputState::new();
        let term = Rect::new(0, 0, 80, 24);
        let metrics = compute_layout_metrics(term, &mut input_state, 1);

        assert!(!metrics.show_tab_bar);
        // Single tab: 2 constraints (message area + input box).
        assert_eq!(metrics.constraints.len(), 2);
        assert_eq!(metrics.input_content_width, 78);
        assert_eq!(metrics.content_width, 78);
        // With a 24-row terminal, 3-row input box (1 line + 2 borders), message
        // area gets 24 - 3 = 21 rows, minus 2 borders = 19 visible lines.
        assert_eq!(metrics.msg_area_height, 19);
    }

    #[test]
    fn layout_metrics_multi_tab() {
        let mut input_state = InputState::new();
        let term = Rect::new(0, 0, 80, 24);
        let metrics = compute_layout_metrics(term, &mut input_state, 3);

        assert!(metrics.show_tab_bar);
        // Multi-tab: 3 constraints (tab bar + message area + input box).
        assert_eq!(metrics.constraints.len(), 3);
        // Tab bar takes 1 row, so message area = 24 - 1 - 3 = 20, minus 2 = 18.
        assert_eq!(metrics.msg_area_height, 18);
    }

    #[test]
    fn layout_metrics_narrow_terminal() {
        let mut input_state = InputState::new();
        let term = Rect::new(0, 0, 20, 10);
        let metrics = compute_layout_metrics(term, &mut input_state, 1);

        assert_eq!(metrics.input_content_width, 18);
        assert_eq!(metrics.content_width, 18);
    }

    #[test]
    fn layout_metrics_cursor_scroll_down() {
        let mut input_state = InputState::new();
        // Simulate a multi-line input that exceeds MAX_INPUT_LINES.
        input_state.input = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8".into();
        input_state.cursor_pos = input_state.input.len(); // cursor at end

        let term = Rect::new(0, 0, 80, 24);
        let metrics = compute_layout_metrics(term, &mut input_state, 1);

        // With 8 lines of input, visible_input_lines is capped at MAX_INPUT_LINES (6).
        assert_eq!(metrics.visible_input_lines, 6);
        // input_row_scroll should have been adjusted so the cursor is visible.
        assert!(metrics.cursor_row < input_state.input_row_scroll + metrics.visible_input_lines);
    }

    #[test]
    fn layout_metrics_cursor_scroll_up() {
        let mut input_state = InputState::new();
        input_state.input = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8".into();
        input_state.cursor_pos = 0; // cursor at start
        input_state.input_row_scroll = 5; // scrolled past cursor

        let term = Rect::new(0, 0, 80, 24);
        let _metrics = compute_layout_metrics(term, &mut input_state, 1);

        // Scroll should have been adjusted back so cursor (row 0) is visible.
        assert_eq!(input_state.input_row_scroll, 0);
    }

    #[test]
    fn layout_metrics_empty_input() {
        let mut input_state = InputState::new();
        let term = Rect::new(0, 0, 80, 24);
        let metrics = compute_layout_metrics(term, &mut input_state, 1);

        assert_eq!(metrics.cursor_row, 0);
        assert_eq!(metrics.cursor_col, 0);
        assert_eq!(metrics.visible_input_lines, 1);
        assert_eq!(metrics.total_input_rows, 1);
    }

    #[test]
    fn layout_metrics_minimum_terminal() {
        let mut input_state = InputState::new();
        // Very small terminal — 10x5.
        let term = Rect::new(0, 0, 10, 5);
        let metrics = compute_layout_metrics(term, &mut input_state, 1);

        assert_eq!(metrics.input_content_width, 8);
        // 5 rows - 3 (input box) = 2, minus 2 borders = 0. But Layout::Min(3)
        // guarantees at least 3 rows for the message area, so height = 3 - 2 = 1.
        assert_eq!(metrics.msg_area_height, 1);
    }
}
