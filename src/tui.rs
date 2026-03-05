use std::io;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

use crate::message::Message;

pub async fn run(
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut write_half: tokio::net::unix::OwnedWriteHalf,
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
    let mut input = String::new();
    let mut scroll_offset: usize = 0;
    let mut result: anyhow::Result<()> = Ok(());

    'main: loop {
        // Drain pending messages from the socket reader
        while let Ok(msg) = msg_rx.try_recv() {
            messages.push(msg);
        }

        let msg_items: Vec<ListItem> = messages
            .iter()
            .map(|m| ListItem::new(format_message(m)))
            .collect();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(3)])
                .split(f.area());

            let visible_count = chunks[0].height.saturating_sub(2) as usize;
            let total = msg_items.len();
            let start = if scroll_offset < total {
                total.saturating_sub(visible_count + scroll_offset)
            } else {
                0
            };
            let visible: Vec<ListItem> = msg_items[start..].to_vec();

            let msg_list = List::new(visible).block(
                Block::default()
                    .title(" room ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            f.render_widget(msg_list, chunks[0]);

            let input_widget = Paragraph::new(input.as_str())
                .block(
                    Block::default()
                        .title(format!(" {username} "))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                )
                .style(Style::default().fg(Color::White));
            f.render_widget(input_widget, chunks[1]);
        })?;

        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Enter => {
                        if !input.is_empty() {
                            let payload = build_payload(&input);
                            input.clear();
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
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break 'main;
                    }
                    KeyCode::Char(c) => {
                        input.push(c);
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Up => {
                        scroll_offset = scroll_offset.saturating_add(1);
                    }
                    KeyCode::Down => {
                        scroll_offset = scroll_offset.saturating_sub(1);
                    }
                    KeyCode::PageUp => {
                        scroll_offset = scroll_offset.saturating_add(10);
                    }
                    KeyCode::PageDown => {
                        scroll_offset = scroll_offset.saturating_sub(10);
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        // Drain any messages that arrived during the poll
        while let Ok(msg) = msg_rx.try_recv() {
            messages.push(msg);
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn format_message(msg: &Message) -> Line<'static> {
    match msg {
        Message::Join { ts, user, .. } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user} joined"),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::ITALIC),
                ),
            ])
        }
        Message::Leave { ts, user, .. } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user} left"),
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::ITALIC),
                ),
            ])
        }
        Message::Message {
            ts, user, content, ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user}: "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(content.clone()),
            ])
        }
        Message::Reply {
            ts,
            user,
            reply_to,
            content,
            ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            let short_id = &reply_to[..reply_to.len().min(8)];
            Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user}: "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("(re:{short_id}) "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(content.clone()),
            ])
        }
        Message::Command {
            ts,
            user,
            cmd,
            params,
            ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user}: "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("/{cmd} {}", params.join(" ")),
                    Style::default().fg(Color::Magenta),
                ),
            ])
        }
        Message::System { ts, content, .. } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("[system] {content}"),
                    Style::default().fg(Color::Cyan),
                ),
            ])
        }
        Message::DirectMessage {
            ts,
            user,
            to,
            content,
            ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("[dm] {user}→{to}: "),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(content.clone()),
            ])
        }
    }
}

/// Convert TUI input to a JSON envelope for the broker.
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
