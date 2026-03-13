//! Extracted terminal draw logic for the TUI.
//!
//! The [`draw_frame`] function contains the rendering code previously inlined
//! inside the `terminal.draw(|f| { ... })` closure in [`super::run`].  All
//! data the closure captured is now bundled into [`DrawContext`].

use std::collections::HashMap;

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};
use room_protocol::SubscriptionTier;

use super::input::InputState;
use super::render::{
    build_member_panel_spans, ellipsize_status, find_view_start, member_panel_row_width,
    render_tab_bar, user_color, welcome_splash, ColorMap, TabInfo,
};
use crate::message::Message;

/// All data required by [`draw_frame`] to render a single terminal frame.
///
/// Fields are borrowed from the main TUI loop state to avoid cloning large
/// buffers on every frame.
pub(super) struct DrawContext<'a> {
    pub(super) constraints: &'a [Constraint],
    pub(super) show_tab_bar: bool,
    pub(super) tab_infos: &'a [TabInfo],
    pub(super) msg_texts: &'a [Text<'static>],
    pub(super) heights: &'a [usize],
    pub(super) total_lines: usize,
    pub(super) scroll_offset: usize,
    pub(super) msg_area_height: usize,
    pub(super) room_id_display: &'a str,
    pub(super) messages: &'a [Message],
    pub(super) online_users: &'a [String],
    pub(super) user_statuses: &'a HashMap<String, String>,
    pub(super) subscription_tiers: &'a HashMap<String, SubscriptionTier>,
    pub(super) color_map: &'a ColorMap,
    pub(super) input_state: &'a InputState,
    pub(super) input_display_rows: &'a [String],
    pub(super) visible_input_lines: usize,
    pub(super) total_input_rows: usize,
    pub(super) cursor_row: usize,
    pub(super) cursor_col: usize,
    pub(super) username: &'a str,
    pub(super) frame_count: usize,
    pub(super) splash_seed: u64,
}

/// Render a single TUI frame.
///
/// This draws (in order):
/// 1. Tab bar (when multiple rooms are open)
/// 2. Message list *or* welcome splash
/// 3. Input box with cursor
/// 4. Floating member-status panel (top-right of message area)
/// 5. Command palette popup
/// 6. Mention picker popup
pub(super) fn draw_frame(f: &mut Frame, ctx: &DrawContext<'_>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(ctx.constraints.to_vec())
        .split(f.area());

    let (tab_bar_chunk, msg_chunk, input_chunk) = if ctx.show_tab_bar {
        (Some(chunks[0]), chunks[1], chunks[2])
    } else {
        (None, chunks[0], chunks[1])
    };

    // ── Tab bar ──────────────────────────────────────────────────────
    if let Some(bar_area) = tab_bar_chunk {
        if let Some(bar_line) = render_tab_bar(ctx.tab_infos) {
            let bar_widget = Paragraph::new(bar_line).style(Style::default().bg(Color::Black));
            f.render_widget(bar_widget, bar_area);
        }
    }

    // ── Message viewport ─────────────────────────────────────────────
    let view_bottom = ctx.total_lines.saturating_sub(ctx.scroll_offset);
    let view_top = view_bottom.saturating_sub(ctx.msg_area_height);

    let (start_msg_idx, skip_first) = find_view_start(ctx.heights, view_top);

    let mut visible: Vec<ListItem> = Vec::new();
    let mut lines_remaining = ctx.msg_area_height;
    for (i, text) in ctx.msg_texts[start_msg_idx..].iter().enumerate() {
        if lines_remaining == 0 {
            break;
        }
        let lines = if i == 0 && skip_first > 0 {
            &text.lines[skip_first..]
        } else {
            &text.lines[..]
        };
        let line_count = lines.len().max(1);
        if line_count <= lines_remaining {
            visible.push(ListItem::new(Text::from(lines.to_vec())));
            lines_remaining -= line_count;
        } else {
            // Partially visible message at viewport bottom — show
            // only the lines that fit instead of hiding the entire
            // message.
            visible.push(ListItem::new(Text::from(lines[..lines_remaining].to_vec())));
            lines_remaining = 0;
        }
    }

    let title = if ctx.scroll_offset > 0 {
        format!(" {} [↑ {} lines] ", ctx.room_id_display, ctx.scroll_offset)
    } else {
        format!(" {} ", ctx.room_id_display)
    };

    // Show the welcome splash when there are no chat messages yet.
    let has_chat = ctx.messages.iter().any(|m| {
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
        let splash_height = msg_chunk.height.saturating_sub(2) as usize;
        let splash = welcome_splash(
            ctx.frame_count,
            splash_width,
            splash_height,
            ctx.splash_seed,
        );
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

    // ── Input box ────────────────────────────────────────────────────
    let end =
        (ctx.input_state.input_row_scroll + ctx.visible_input_lines).min(ctx.total_input_rows);
    let display_text = ctx.input_display_rows[ctx.input_state.input_row_scroll..end].join("\n");

    let username = ctx.username;
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
    let visible_cursor_row = ctx.cursor_row - ctx.input_state.input_row_scroll;
    let cursor_x = input_chunk.x + 1 + ctx.cursor_col as u16;
    let cursor_y = input_chunk.y + 1 + visible_cursor_row as u16;
    f.set_cursor_position((cursor_x, cursor_y));

    // ── Floating member-status panel (top-right of message area) ─────
    const PANEL_MIN_TERM_WIDTH: u16 = 80;
    if f.area().width >= PANEL_MIN_TERM_WIDTH && !ctx.online_users.is_empty() {
        // Compute the ideal panel width from raw (untruncated) statuses,
        // then cap it. Re-render items with ellipsized statuses that fit
        // within the capped inner width.
        let panel_content_width = ctx
            .online_users
            .iter()
            .map(|u| {
                let status = ctx.user_statuses.get(u).map(|s| s.as_str()).unwrap_or("");
                let tier = ctx.subscription_tiers.get(u).copied();
                member_panel_row_width(u, status, tier)
            })
            .max()
            .unwrap_or(10);
        let panel_width = (panel_content_width as u16 + 2)
            .min(msg_chunk.width / 3)
            .max(12);
        // Inner width available for content (excluding left+right border).
        let inner_width = panel_width.saturating_sub(2) as usize;

        let panel_items: Vec<ListItem> =
            ctx.online_users
                .iter()
                .map(|u| {
                    let raw_status = ctx.user_statuses.get(u).map(|s| s.as_str()).unwrap_or("");
                    let tier = ctx.subscription_tiers.get(u).copied();
                    // Compute how many chars are available for status text:
                    // inner_width - " " (1) - username - tier_indicator - "  " (2 before status) - " " (1 trailing)
                    let tier_len: usize = match tier {
                        Some(SubscriptionTier::MentionsOnly)
                        | Some(SubscriptionTier::Unsubscribed) => 2,
                        _ => 0,
                    };
                    let overhead = 1 + u.len() + tier_len + 1; // leading space + name + tier + trailing space
                    let max_status_chars = if !raw_status.is_empty() {
                        // +2 for the "  " prefix before the status text
                        inner_width.saturating_sub(overhead + 2)
                    } else {
                        0
                    };
                    let status = if !raw_status.is_empty() {
                        ellipsize_status(raw_status, max_status_chars)
                    } else {
                        String::new()
                    };
                    let spans = build_member_panel_spans(u, &status, tier, ctx.color_map);
                    ListItem::new(Line::from(spans))
                })
                .collect();
        let panel_height =
            (ctx.online_users.len() as u16 + 2).min(msg_chunk.height.saturating_sub(1));

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

    // ── Command palette popup ────────────────────────────────────────
    if ctx.input_state.palette.active && !ctx.input_state.palette.filtered.is_empty() {
        let palette_items: Vec<ListItem> = ctx
            .input_state
            .palette
            .filtered
            .iter()
            .enumerate()
            .map(|(row, &idx)| {
                let item = &ctx.input_state.palette.commands[idx];
                let style = if row == ctx.input_state.palette.selected {
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
                        if row == ctx.input_state.palette.selected {
                            Style::default().fg(Color::Black).bg(Color::Cyan)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                ]))
            })
            .collect();

        let popup_height =
            (ctx.input_state.palette.filtered.len() as u16 + 2).min(msg_chunk.height);
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

    // ── Mention picker popup ─────────────────────────────────────────
    if ctx.input_state.mention.active && !ctx.input_state.mention.filtered.is_empty() {
        let mention_items: Vec<ListItem> = ctx
            .input_state
            .mention
            .filtered
            .iter()
            .enumerate()
            .map(|(row, user)| {
                let is_cross = ctx.input_state.mention.is_cross_room(row);
                let is_selected = row == ctx.input_state.mention.selected;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(user_color(user, ctx.color_map))
                        .add_modifier(Modifier::BOLD)
                } else if is_cross {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(user_color(user, ctx.color_map))
                };
                let label = if is_cross && !is_selected {
                    format!("@{user} \u{2022}")
                } else {
                    format!("@{user}")
                };
                ListItem::new(Line::from(Span::styled(label, style)))
            })
            .collect();

        let popup_height =
            (ctx.input_state.mention.filtered.len() as u16 + 2).min(msg_chunk.height);
        let popup_y = input_chunk.y.saturating_sub(popup_height);
        let max_width = ctx
            .input_state
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
}
