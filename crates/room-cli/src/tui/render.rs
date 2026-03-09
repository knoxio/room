use std::collections::{HashMap, HashSet};

use chrono::Local;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

use crate::message::Message;

/// Color palette for user names.
const PALETTE: &[Color] = &[
    Color::Yellow,
    Color::Cyan,
    Color::Green,
    Color::Magenta,
    Color::LightYellow,
    Color::LightCyan,
    Color::LightGreen,
    Color::LightMagenta,
    Color::LightRed,
    Color::LightBlue,
];

/// Persistent map of username -> assigned color. Stored in TUI state.
pub(super) type ColorMap = HashMap<String, Color>;

/// Assign a color to a username, preferring unused palette colors.
///
/// If the user already has a color, returns it. Otherwise picks the
/// hash-preferred color if available, or the first unused palette color.
/// Falls back to the hash color when all palette slots are taken.
pub(super) fn assign_color(username: &str, color_map: &mut ColorMap) -> Color {
    if let Some(&color) = color_map.get(username) {
        return color;
    }
    let used: HashSet<Color> = color_map.values().copied().collect();
    let hash = username.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as usize)
    });
    let preferred = PALETTE[hash % PALETTE.len()];
    if !used.contains(&preferred) {
        color_map.insert(username.to_owned(), preferred);
        return preferred;
    }
    // Hash color is taken — find first unused palette color.
    for &color in PALETTE {
        if !used.contains(&color) {
            color_map.insert(username.to_owned(), color);
            return color;
        }
    }
    // All palette colors used — accept collision with hash color.
    color_map.insert(username.to_owned(), preferred);
    preferred
}

/// Arrow glyph used in DM display (`→`).
const DM_ARROW: &str = "\u{2192}";

/// Antenna light blink rate (~500 ms at 50 ms/frame).
const BLINK_INTERVAL: usize = 10;
/// Slot length in frames (~1 s). Each slot independently rolls for events.
const SLOT_FRAMES: usize = 20;
/// Probability (out of 100) that a slot triggers a wink/blink (~15% → avg every ~6 s).
const WINK_CHANCE: u64 = 15;
/// How many frames into a slot the wink lasts (~250 ms).
const WINK_DURATION: usize = 5;
/// Probability (out of 100) that a slot triggers a talking burst (~10% → avg every ~10 s).
const TALK_CHANCE: u64 = 10;
/// How many frames into a slot the talking burst lasts (~1 s).
const TALK_DURATION: usize = 20;
/// Rapid mouth alternation rate during a talking burst (~150 ms).
const TALK_INTERVAL: usize = 3;

// ── Generative bot components ─────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum BotBorder {
    Round,
    Square,
    Double,
    Heavy,
}

#[derive(Clone, Copy)]
enum BotEyes {
    Circle,
    Dot,
    Block,
    Ring,
    Diamond,
}

#[derive(Clone, Copy)]
enum WinkStyle {
    Left,
    Right,
    Blink,
}

#[derive(Clone, Copy)]
enum BotMouth {
    Smile,
    Flat,
    Dots,
    Wave,
    Teeth,
    TriDown,
    Stern,
    Surprised,
    NeutralDot,
    Dashed,
    WavyAlt,
}

struct BotParts {
    seed: u64,
    border: BotBorder,
    eyes: BotEyes,
    mouth: BotMouth,
    wink_style: WinkStyle,
}

/// Deterministically generate bot parts from a 64-bit seed (splitmix64).
fn bot_from_seed(seed: u64) -> BotParts {
    let mut s = seed.wrapping_add(0x9e3779b97f4a7c15);
    s = (s ^ (s >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    s = (s ^ (s >> 27)).wrapping_mul(0x94d049bb133111eb);
    s ^= s >> 31;

    let border = match s & 3 {
        0 => BotBorder::Round,
        1 => BotBorder::Square,
        2 => BotBorder::Double,
        _ => BotBorder::Heavy,
    };
    let eyes = match (s >> 2) % 5 {
        0 => BotEyes::Circle,
        1 => BotEyes::Dot,
        2 => BotEyes::Block,
        3 => BotEyes::Ring,
        _ => BotEyes::Diamond,
    };
    let mouth = match (s >> 5) % 11 {
        0 => BotMouth::Smile,
        1 => BotMouth::Flat,
        2 => BotMouth::Dots,
        3 => BotMouth::Wave,
        4 => BotMouth::Teeth,
        5 => BotMouth::TriDown,
        6 => BotMouth::Stern,
        7 => BotMouth::Surprised,
        8 => BotMouth::NeutralDot,
        9 => BotMouth::Dashed,
        _ => BotMouth::WavyAlt,
    };
    let wink_style = match (s >> 10) % 3 {
        0 => WinkStyle::Left,
        1 => WinkStyle::Right,
        _ => WinkStyle::Blink,
    };
    BotParts {
        seed,
        border,
        eyes,
        mouth,
        wink_style,
    }
}

/// Hash a (bot_seed, slot) pair to get a random value for that time slot.
/// Used to decide independently per-slot whether a wink or talk event fires.
fn slot_hash(bot_seed: u64, slot: usize) -> u64 {
    let mut s = bot_seed.wrapping_add((slot as u64).wrapping_mul(0x9e3779b97f4a7c15));
    s = (s ^ (s >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    s = (s ^ (s >> 27)).wrapping_mul(0x94d049bb133111eb);
    s ^ (s >> 31)
}

fn bot_color(border: BotBorder) -> Color {
    match border {
        BotBorder::Round => Color::Cyan,
        BotBorder::Square => Color::Green,
        BotBorder::Double => Color::Magenta,
        BotBorder::Heavy => Color::Yellow,
    }
}

/// Returns (tl, tr, bl, br, fill, side, neck) box-drawing characters.
fn border_chars(
    border: BotBorder,
) -> (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
) {
    match border {
        BotBorder::Round => ("╭", "╮", "╰", "╯", "─", "│", "┬"),
        BotBorder::Square => ("┌", "┐", "└", "┘", "─", "│", "┬"),
        BotBorder::Double => ("╔", "╗", "╚", "╝", "═", "║", "╤"),
        BotBorder::Heavy => ("┏", "┓", "┗", "┛", "━", "┃", "┳"),
    }
}

fn eye_char(eyes: BotEyes) -> &'static str {
    match eyes {
        BotEyes::Circle => "◉",
        BotEyes::Dot => "•",
        BotEyes::Block => "■",
        BotEyes::Ring => "◎",
        BotEyes::Diamond => "◈",
    }
}

fn mouth_str(mouth: BotMouth) -> &'static str {
    match mouth {
        BotMouth::Smile => "╰─╯",
        BotMouth::Flat => "───",
        BotMouth::Dots => "···",
        BotMouth::Wave => "∿∿∿",
        BotMouth::Teeth => "▾▾▾",
        BotMouth::TriDown => "▿▿▿",
        BotMouth::Stern => "═══",
        BotMouth::Surprised => "o─o",
        BotMouth::NeutralDot => "─·─",
        BotMouth::Dashed => "╌╌╌",
        BotMouth::WavyAlt => "≈≈≈",
    }
}

/// Correlated talking state for each mouth — same character family, mouth open.
/// `Surprised` is already open so it closes slightly when "talking".
fn mouth_talk_str(mouth: BotMouth) -> &'static str {
    match mouth {
        BotMouth::Smile => "╰o╯",
        BotMouth::Flat => "─o─",
        BotMouth::Dots => "·o·",
        BotMouth::Wave => "∿o∿",
        BotMouth::Teeth => "▾o▾",
        BotMouth::TriDown => "▿o▿",
        BotMouth::Stern => "═o═",
        BotMouth::Surprised => "───",
        BotMouth::NeutralDot => "─o─",
        BotMouth::Dashed => "╌o╌",
        BotMouth::WavyAlt => "≈o≈",
    }
}

/// Render one row (0–5) of a 9-column-wide bot face.
///
/// ```text
///   row 0 — antenna light   "    ✦    "
///   row 1 — antenna stem    "    │    "
///   row 2 — top border      "╭───────╮"
///   row 3 — eyes            "│ ◉   ◉ │"
///   row 4 — mouth           "│  ╰─╯  │"
///   row 5 — bottom + neck   "╰───┬───╯"
/// ```
fn bot_row(parts: &BotParts, row: usize, frame: usize) -> (String, Style) {
    let antenna_on = (frame / BLINK_INTERVAL).is_multiple_of(2);
    let slot = frame / SLOT_FRAMES;
    let frame_in_slot = frame % SLOT_FRAMES;
    let h = slot_hash(parts.seed, slot);
    let wink = (h % 100 < WINK_CHANCE) && (frame_in_slot < WINK_DURATION);
    let talking = ((h >> 16) % 100 < TALK_CHANCE) && (frame_in_slot < TALK_DURATION);
    let color = bot_color(parts.border);
    let (tl, tr, bl, br, fill, side, neck) = border_chars(parts.border);

    let text = match row {
        0 => {
            let ch = if antenna_on { "✦" } else { "·" };
            format!("    {ch}    ")
        }
        1 => "    │    ".to_string(),
        2 => format!("{tl}{}{tr}", fill.repeat(7)),
        3 => {
            let eye = eye_char(parts.eyes);
            let (le, re) = if wink {
                match parts.wink_style {
                    WinkStyle::Left => ("─", eye),
                    WinkStyle::Right => (eye, "─"),
                    WinkStyle::Blink => ("─", "─"),
                }
            } else {
                (eye, eye)
            };
            format!("{side} {le}   {re} {side}")
        }
        4 => {
            let m = if talking {
                if (frame_in_slot / TALK_INTERVAL).is_multiple_of(2) {
                    mouth_str(parts.mouth)
                } else {
                    mouth_talk_str(parts.mouth)
                }
            } else {
                mouth_str(parts.mouth)
            };
            format!("{side}  {m}  {side}")
        }
        5 => format!("{bl}{}{neck}{}{br}", fill.repeat(3), fill.repeat(3)),
        _ => unreachable!(),
    };

    let style = if row == 0 {
        if antenna_on {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    } else {
        Style::default().fg(color)
    };

    (text, style)
}

/// Build the welcome splash as centered, styled `Text`.
///
/// `frame` drives per-bot animations with staggered phase offsets so the
/// group never blinks in unison.  `seed` is set once at TUI startup and
/// used to deterministically assemble a unique bot face (border, eyes, mouth)
/// for every slot — the same session always looks the same, but two sessions
/// are almost certainly different.
///
/// Bot count adapts to `width`:
///   < 55  → 1 bot
///   55–99 → 3 bots
///   ≥ 100 → 5 bots
pub(super) fn welcome_splash(frame: usize, width: usize, seed: u64) -> Text<'static> {
    let bot_count: usize = if width >= 100 {
        5
    } else if width >= 55 {
        3
    } else {
        1
    };
    const GAP: &str = "   "; // 3 spaces between adjacent bots

    let bots: Vec<BotParts> = (0..bot_count)
        .map(|i| bot_from_seed(seed.wrapping_add(i as u64 * 6364136223846793005)))
        .collect();

    // Stagger by 7 frames so bots animate out-of-phase with each other.
    let frame_for = |i: usize| frame.wrapping_add(i * 7);

    // Total display width of the bot group.
    let group_width = bot_count * 9 + bot_count.saturating_sub(1) * GAP.len();

    let mut lines: Vec<Line<'static>> = vec![Line::from("")]; // top padding

    for row in 0..6 {
        let pad = " ".repeat(width.saturating_sub(group_width) / 2);
        let mut spans: Vec<Span<'static>> = Vec::new();
        if !pad.is_empty() {
            spans.push(Span::raw(pad));
        }
        for (i, bot) in bots.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(GAP));
            }
            let (text, style) = bot_row(bot, row, frame_for(i));
            spans.push(Span::styled(text, style));
        }
        lines.push(Line::from(spans));
    }

    // "r o o m" label pulses in sync with the antenna.
    let label_style = if (frame / BLINK_INTERVAL).is_multiple_of(2) {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    lines.push(Line::from(""));
    let label = "r o o m";
    let label_pad = " ".repeat(width.saturating_sub(label.len()) / 2);
    lines.push(Line::from(vec![
        Span::raw(label_pad),
        Span::styled(label, label_style),
    ]));

    lines.push(Line::from(""));

    let tagline = "Agent coordination for humans";
    let tagline_pad = " ".repeat(width.saturating_sub(tagline.len()) / 2);
    lines.push(Line::from(vec![
        Span::raw(tagline_pad),
        Span::styled(tagline, Style::default().fg(Color::DarkGray)),
    ]));

    let version = env!("CARGO_PKG_VERSION");
    let today = Local::now().format("%Y-%m-%d");
    let info = format!("v{version}  ·  {today}");
    let info_pad = " ".repeat(width.saturating_sub(info.len()) / 2);
    lines.push(Line::from(vec![
        Span::raw(info_pad),
        Span::styled(info, Style::default().fg(Color::DarkGray)),
    ]));

    Text::from(lines)
}

/// Split a message content string into styled spans, rendering `@username`
/// tokens in the mentioned user's colour.
pub(super) fn render_content_with_mentions(
    content: &str,
    color_map: &ColorMap,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut remaining = content;
    while let Some(at_pos) = remaining.find('@') {
        if at_pos > 0 {
            spans.push(Span::raw(remaining[..at_pos].to_string()));
        }
        let after_at = &remaining[at_pos + 1..];
        let username_end = after_at
            .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .unwrap_or(after_at.len());
        if username_end == 0 {
            // Bare '@' with no username — treat as literal text.
            spans.push(Span::raw("@".to_string()));
            remaining = after_at;
        } else {
            let username = &after_at[..username_end];
            spans.push(Span::styled(
                format!("@{username}"),
                Style::default()
                    .fg(user_color(username, color_map))
                    .add_modifier(Modifier::BOLD),
            ));
            remaining = &after_at[username_end..];
        }
    }
    if !remaining.is_empty() {
        spans.push(Span::raw(remaining.to_string()));
    }
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

/// Word-wrap `text` so that no line exceeds `width` characters.
///
/// Explicit `\n` characters in `text` are preserved as hard line breaks.
/// Words longer than `width` are hard-split at the column boundary.
/// If `width` is 0 the text is returned as a single unsplit chunk.
pub(super) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut all_chunks: Vec<String> = Vec::new();
    for logical_line in text.split('\n') {
        let mut current = String::new();
        for word in logical_line.split_whitespace() {
            if current.is_empty() {
                // Hard-split any word that is longer than the available width.
                let mut w = word;
                while w.chars().count() > width {
                    let split_idx = w
                        .char_indices()
                        .nth(width)
                        .map(|(i, _)| i)
                        .unwrap_or(w.len());
                    all_chunks.push(w[..split_idx].to_string());
                    w = &w[split_idx..];
                }
                current = w.to_string();
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                all_chunks.push(std::mem::take(&mut current));
                let mut w = word;
                while w.chars().count() > width {
                    let split_idx = w
                        .char_indices()
                        .nth(width)
                        .map(|(i, _)| i)
                        .unwrap_or(w.len());
                    all_chunks.push(w[..split_idx].to_string());
                    w = &w[split_idx..];
                }
                current = w.to_string();
            }
        }
        // Push remaining content for this logical line (may be empty for blank lines).
        all_chunks.push(current);
    }
    all_chunks
}

pub(super) fn format_message(
    msg: &Message,
    available_width: usize,
    color_map: &ColorMap,
) -> Text<'static> {
    match msg {
        Message::Join { ts, user, .. } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Text::from(Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user} joined"),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]))
        }
        Message::Leave { ts, user, .. } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Text::from(Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user} left"),
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]))
        }
        Message::Message {
            ts, user, content, ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            let prefix_plain = format!("[{ts_str}] {user}: ");
            let prefix_width = prefix_plain.chars().count();
            let content_width = available_width.saturating_sub(prefix_width);
            let chunks = wrap_words(content, content_width);
            let indent = " ".repeat(prefix_width);
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i == 0 {
                    let mut line_spans = vec![
                        Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("{user}: "),
                            Style::default()
                                .fg(user_color(user, color_map))
                                .add_modifier(Modifier::BOLD),
                        ),
                    ];
                    line_spans.extend(render_content_with_mentions(&chunk, color_map));
                    lines.push(Line::from(line_spans));
                } else {
                    let mut line_spans = vec![Span::raw(indent.clone())];
                    line_spans.extend(render_content_with_mentions(&chunk, color_map));
                    lines.push(Line::from(line_spans));
                }
            }
            Text::from(lines)
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
            let prefix_plain = format!("[{ts_str}] {user}: (re:{short_id}) ");
            let prefix_width = prefix_plain.chars().count();
            let content_width = available_width.saturating_sub(prefix_width);
            let chunks = wrap_words(content, content_width);
            let indent = " ".repeat(prefix_width);
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i == 0 {
                    let mut line_spans = vec![
                        Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("{user}: "),
                            Style::default()
                                .fg(user_color(user, color_map))
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("(re:{short_id}) "),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ];
                    line_spans.extend(render_content_with_mentions(&chunk, color_map));
                    lines.push(Line::from(line_spans));
                } else {
                    let mut line_spans = vec![Span::raw(indent.clone())];
                    line_spans.extend(render_content_with_mentions(&chunk, color_map));
                    lines.push(Line::from(line_spans));
                }
            }
            Text::from(lines)
        }
        Message::Command {
            ts,
            user,
            cmd,
            params,
            ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            Text::from(Line::from(vec![
                Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{user}: "),
                    Style::default()
                        .fg(user_color(user, color_map))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("/{cmd} {}", params.join(" ")),
                    Style::default().fg(Color::Magenta),
                ),
            ]))
        }
        Message::System { ts, content, .. } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            let prefix_plain = format!("[{ts_str}] [system] ");
            let prefix_width = prefix_plain.chars().count();
            let content_width = available_width.saturating_sub(prefix_width);
            let chunks = wrap_words(content, content_width);
            let indent = " ".repeat(prefix_width);
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("[system] {chunk}"),
                            Style::default().fg(Color::Cyan),
                        ),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw(indent.clone()),
                        Span::styled(chunk, Style::default().fg(Color::Cyan)),
                    ]));
                }
            }
            Text::from(lines)
        }
        Message::DirectMessage {
            ts,
            user,
            to,
            content,
            ..
        } => {
            let ts_str = ts.format("%H:%M:%S").to_string();
            let prefix_plain = format!("[{ts_str}] [dm] {user}{DM_ARROW}{to}: ");
            let prefix_width = prefix_plain.chars().count();
            let content_width = available_width.saturating_sub(prefix_width);
            let chunks = wrap_words(content, content_width);
            let indent = " ".repeat(prefix_width);
            let mut lines: Vec<Line<'static>> = Vec::new();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(format!("[{ts_str}] "), Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            "[dm] ",
                            Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{user}{DM_ARROW}{to}: "),
                            Style::default()
                                .fg(user_color(user, color_map))
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(chunk),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw(indent.clone()),
                        Span::raw(chunk),
                    ]));
                }
            }
            Text::from(lines)
        }
    }
}

/// Given per-message visual line heights and a target top line index,
/// returns `(message_index, lines_to_skip)` — the first message that
/// should appear in the viewport and how many of its leading lines to drop.
pub(super) fn find_view_start(heights: &[usize], view_top: usize) -> (usize, usize) {
    let mut accum = 0usize;
    for (i, &h) in heights.iter().enumerate() {
        if accum + h > view_top {
            return (i, view_top - accum);
        }
        accum += h;
    }
    (heights.len(), 0)
}

/// Describes a single tab for the tab bar renderer.
pub(super) struct TabInfo {
    pub(super) room_id: String,
    pub(super) active: bool,
    pub(super) unread: usize,
}

/// Render the tab bar as a single `Line` of styled spans.
///
/// Hidden when there is only one tab (backward-compatible single-room mode).
/// Active tab is highlighted; inactive tabs with unread messages show a count badge.
pub(super) fn render_tab_bar(tabs: &[TabInfo]) -> Option<Line<'static>> {
    if tabs.len() <= 1 {
        return None;
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));
    for tab in tabs {
        let label = if tab.unread > 0 && !tab.active {
            format!(" {} ({}) ", tab.room_id, tab.unread)
        } else {
            format!(" {} ", tab.room_id)
        };
        let style = if tab.active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if tab.unread > 0 {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    Some(Line::from(spans))
}

/// Look up a username's color from the map, falling back to the hash-based
/// palette index if the user has not been assigned a color yet.
pub(super) fn user_color(username: &str, color_map: &ColorMap) -> Color {
    if let Some(&color) = color_map.get(username) {
        return color;
    }
    let hash = username.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as usize)
    });
    PALETTE[hash % PALETTE.len()]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── wrap_words tests ──────────────────────────────────────────────────────

    #[test]
    fn wrap_words_preserves_explicit_newline() {
        let chunks = wrap_words("hello\nworld", 40);
        assert_eq!(chunks, ["hello", "world"]);
    }

    #[test]
    fn wrap_words_double_newline_produces_blank_line() {
        let chunks = wrap_words("first\n\nsecond", 40);
        assert_eq!(chunks, ["first", "", "second"]);
    }

    #[test]
    fn wrap_words_newline_with_wrapping() {
        // Each logical line is word-wrapped independently.
        let chunks = wrap_words("short\na b c d e", 5);
        // "short" fits in 5 chars; "a b c d e" wraps as "a b c", "d e"
        assert_eq!(chunks, ["short", "a b c", "d e"]);
    }

    #[test]
    fn wrap_words_no_newline_unchanged() {
        let chunks = wrap_words("hello world", 40);
        assert_eq!(chunks, ["hello world"]);
    }

    #[test]
    fn wrap_words_trailing_newline_produces_blank_chunk() {
        let chunks = wrap_words("hello\n", 40);
        assert_eq!(chunks, ["hello", ""]);
    }

    // ── render_content_with_mentions ─────────────────────────────────────────

    #[test]
    fn render_mentions_no_at_returns_single_raw_span() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("hello world", &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world");
    }

    #[test]
    fn render_mentions_bare_at_no_username_is_literal() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("email@", &cm);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "email@");
    }

    #[test]
    fn render_mentions_single_mention() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("hey @alice!", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hey ");
        assert_eq!(spans[1].content, "@alice");
        assert_eq!(spans[2].content, "!");
    }

    #[test]
    fn render_mentions_multiple_mentions() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("@alice and @bob", &cm);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "@alice");
        assert_eq!(spans[1].content, " and ");
        assert_eq!(spans[2].content, "@bob");
    }

    #[test]
    fn render_mentions_mention_only() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("@r2d2", &cm);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "@r2d2");
    }

    #[test]
    fn render_mentions_empty_content() {
        let cm = ColorMap::new();
        let spans = render_content_with_mentions("", &cm);
        assert!(!spans.is_empty());
    }

    // ── assign_color ─────────────────────────────────────────────────────────

    #[test]
    fn assign_color_returns_consistent_color() {
        let mut cm = ColorMap::new();
        let c1 = assign_color("alice", &mut cm);
        let c2 = assign_color("alice", &mut cm);
        assert_eq!(c1, c2);
    }

    #[test]
    fn assign_color_different_users_get_different_colors() {
        let mut cm = ColorMap::new();
        let c1 = assign_color("alice", &mut cm);
        let c2 = assign_color("bob", &mut cm);
        assert_ne!(c1, c2);
    }

    #[test]
    fn assign_color_avoids_collision_when_preferred_taken() {
        let mut cm = ColorMap::new();
        // "alice" gets her preferred color first.
        let alice_color = assign_color("alice", &mut cm);
        // Find another username that hashes to the same palette index.
        let mut collider = String::new();
        for i in 0u32..10_000 {
            let name = format!("u{i}");
            let hash = name.bytes().fold(0usize, |acc, b| {
                acc.wrapping_mul(31).wrapping_add(b as usize)
            });
            if PALETTE[hash % PALETTE.len()] == alice_color {
                collider = name;
                break;
            }
        }
        assert!(!collider.is_empty(), "could not find a colliding username");
        let collider_color = assign_color(&collider, &mut cm);
        // The collider should NOT get Alice's color — it should get a different unused one.
        assert_ne!(collider_color, alice_color);
    }

    #[test]
    fn assign_color_fills_all_palette_slots() {
        let mut cm = ColorMap::new();
        let mut colors = HashSet::new();
        // Assign colors to enough users to fill the palette.
        for i in 0..PALETTE.len() {
            let c = assign_color(&format!("user{i}"), &mut cm);
            colors.insert(c);
        }
        // Every palette color should be used exactly once.
        assert_eq!(colors.len(), PALETTE.len());
    }

    #[test]
    fn assign_color_accepts_collision_when_palette_exhausted() {
        let mut cm = ColorMap::new();
        // Fill all palette slots.
        for i in 0..PALETTE.len() {
            assign_color(&format!("user{i}"), &mut cm);
        }
        // The 11th user must accept a collision.
        let c = assign_color("overflow", &mut cm);
        assert!(PALETTE.contains(&c));
    }

    #[test]
    fn user_color_uses_map_when_present() {
        let mut cm = ColorMap::new();
        cm.insert("alice".to_owned(), Color::LightRed);
        assert_eq!(user_color("alice", &cm), Color::LightRed);
    }

    #[test]
    fn user_color_falls_back_to_hash_when_not_in_map() {
        let cm = ColorMap::new();
        let c = user_color("alice", &cm);
        let hash = "alice".bytes().fold(0usize, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(b as usize)
        });
        assert_eq!(c, PALETTE[hash % PALETTE.len()]);
    }

    // ── find_view_start tests ────────────────────────────────────────────────

    #[test]
    fn find_view_start_single_line_messages() {
        // 5 messages, each 1 line tall
        let heights = [1, 1, 1, 1, 1];
        assert_eq!(find_view_start(&heights, 0), (0, 0));
        assert_eq!(find_view_start(&heights, 2), (2, 0));
        assert_eq!(find_view_start(&heights, 4), (4, 0));
    }

    #[test]
    fn find_view_start_multi_line_message_partial() {
        // msg0: 1 line, msg1: 5 lines, msg2: 1 line = 7 total
        let heights = [1, 5, 1];
        // view_top=0 → start at msg0, skip 0
        assert_eq!(find_view_start(&heights, 0), (0, 0));
        // view_top=1 → start at msg1 (line 1 is first line of msg1), skip 0
        assert_eq!(find_view_start(&heights, 1), (1, 0));
        // view_top=2 → start at msg1, skip 1 line (show lines 2-5 of msg1)
        assert_eq!(find_view_start(&heights, 2), (1, 1));
        // view_top=3 → start at msg1, skip 2 lines
        assert_eq!(find_view_start(&heights, 3), (1, 2));
        // view_top=5 → start at msg1, skip 4 lines (show only last line of msg1)
        assert_eq!(find_view_start(&heights, 5), (1, 4));
        // view_top=6 → start at msg2, skip 0
        assert_eq!(find_view_start(&heights, 6), (2, 0));
    }

    #[test]
    fn find_view_start_past_end_returns_len() {
        let heights = [1, 2];
        // view_top=3 is exactly total_lines → past end
        assert_eq!(find_view_start(&heights, 3), (2, 0));
        // view_top=10 → way past end
        assert_eq!(find_view_start(&heights, 10), (2, 0));
    }

    #[test]
    fn find_view_start_empty_heights() {
        let heights: [usize; 0] = [];
        assert_eq!(find_view_start(&heights, 0), (0, 0));
    }

    // ── welcome_splash tests ──────────────────────────────────────────────

    fn splash_text(frame: usize, width: usize) -> String {
        welcome_splash(frame, width, 0)
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn welcome_splash_contains_tagline() {
        assert!(
            splash_text(0, 60).contains("Agent coordination for humans"),
            "splash should contain tagline"
        );
    }

    #[test]
    fn welcome_splash_contains_version() {
        let version = env!("CARGO_PKG_VERSION");
        assert!(
            splash_text(0, 60).contains(&format!("v{version}")),
            "splash should contain version"
        );
    }

    #[test]
    fn welcome_splash_contains_date() {
        let today = Local::now().format("%Y-%m-%d").to_string();
        assert!(
            splash_text(0, 60).contains(&today),
            "splash should contain today's date"
        );
    }

    #[test]
    fn welcome_splash_animation_changes_between_phases() {
        let a = splash_text(0, 60);
        let b = splash_text(10, 60); // one BLINK_INTERVAL later
        assert_ne!(a, b, "animation should change between phases");
    }

    #[test]
    fn welcome_splash_has_more_lines_than_logo() {
        let text = welcome_splash(0, 60, 0);
        // 1 vpad + 6 bot rows + 1 blank + 1 label + 1 blank + 1 tagline + 1 version = 12
        assert!(
            text.lines.len() >= 11,
            "splash should have logo + tagline + version lines, got {}",
            text.lines.len()
        );
    }

    #[test]
    fn welcome_splash_single_bot_below_threshold() {
        // width=40 < 55 → 1 bot → no inter-bot GAP spans ("   ") should exist.
        // Padding for this width is (40-9)/2 = 15 spaces, which is never confused
        // with the 3-space GAP constant.
        let text = welcome_splash(0, 40, 42);
        let has_gap_span = text
            .lines
            .iter()
            .any(|line| line.spans.iter().any(|s| s.content.as_ref() == "   "));
        assert!(
            !has_gap_span,
            "single bot should have no inter-bot gap spans"
        );
    }

    #[test]
    fn welcome_splash_three_bots_at_medium_width() {
        // width=80 → 3 bots; all 3 bottom-border rows should appear in the same line
        let text = welcome_splash(0, 80, 99);
        // Row 5 is the bottom border line; we should have 2 GAP spans in it
        let bottom_row = text.lines.iter().find(|line| {
            let raw: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            // bottom row contains a neck char from at least one border style
            raw.contains('┬') || raw.contains('╤') || raw.contains('┳')
        });
        assert!(bottom_row.is_some(), "should find a bottom-border row");
    }

    #[test]
    fn welcome_splash_seed_produces_variety() {
        // Two different seeds should produce different bot faces.
        let a = splash_text(0, 80);
        let b = welcome_splash(0, 80, 9999999999)
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("");
        assert_ne!(
            a, b,
            "different seeds should (almost always) produce different bots"
        );
    }

    // ── render_tab_bar tests ──────────────────────────────────────────────

    #[test]
    fn tab_bar_hidden_for_single_tab() {
        let tabs = vec![TabInfo {
            room_id: "room-1".into(),
            active: true,
            unread: 0,
        }];
        assert!(render_tab_bar(&tabs).is_none());
    }

    #[test]
    fn tab_bar_hidden_for_empty_tabs() {
        let tabs: Vec<TabInfo> = vec![];
        assert!(render_tab_bar(&tabs).is_none());
    }

    #[test]
    fn tab_bar_shown_for_multiple_tabs() {
        let tabs = vec![
            TabInfo {
                room_id: "alpha".into(),
                active: true,
                unread: 0,
            },
            TabInfo {
                room_id: "beta".into(),
                active: false,
                unread: 0,
            },
        ];
        let line = render_tab_bar(&tabs).expect("should render tab bar");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("alpha"), "tab bar should contain 'alpha'");
        assert!(text.contains("beta"), "tab bar should contain 'beta'");
    }

    #[test]
    fn tab_bar_shows_unread_badge_on_inactive_tab() {
        let tabs = vec![
            TabInfo {
                room_id: "alpha".into(),
                active: true,
                unread: 0,
            },
            TabInfo {
                room_id: "beta".into(),
                active: false,
                unread: 5,
            },
        ];
        let line = render_tab_bar(&tabs).expect("should render tab bar");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("beta (5)"),
            "inactive tab with unread should show count badge"
        );
    }

    #[test]
    fn tab_bar_no_unread_badge_on_active_tab() {
        let tabs = vec![
            TabInfo {
                room_id: "alpha".into(),
                active: true,
                unread: 3,
            },
            TabInfo {
                room_id: "beta".into(),
                active: false,
                unread: 0,
            },
        ];
        let line = render_tab_bar(&tabs).expect("should render tab bar");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        // Active tab should NOT show unread badge even if count > 0
        assert!(
            !text.contains("alpha (3)"),
            "active tab should not show unread badge"
        );
    }

    #[test]
    fn tab_bar_active_tab_has_bold_cyan_style() {
        let tabs = vec![
            TabInfo {
                room_id: "alpha".into(),
                active: true,
                unread: 0,
            },
            TabInfo {
                room_id: "beta".into(),
                active: false,
                unread: 0,
            },
        ];
        let line = render_tab_bar(&tabs).unwrap();
        // Find the span containing "alpha"
        let alpha_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("alpha"))
            .expect("should find alpha span");
        assert_eq!(alpha_span.style.fg, Some(Color::Black));
        assert_eq!(alpha_span.style.bg, Some(Color::Cyan));
    }

    #[test]
    fn find_view_start_scroll_through_tall_message_line_by_line() {
        // Simulate scrolling through a 10-line message with a 3-line viewport
        let heights = [2, 10, 2]; // total = 14
        let viewport = 3;

        // scroll_offset goes from 0 to total-viewport = 11
        // For each offset, verify the view start advances line by line
        let expected: Vec<(usize, usize)> = vec![
            // scroll_offset=0: vb=14, vt=11 → msg1 skip 9 (last line of 10-line msg)
            (1, 9),
            // scroll_offset=1: vb=13, vt=10 → msg1 skip 8
            (1, 8),
            // scroll_offset=2: vb=12, vt=9 → msg1 skip 7
            (1, 7),
            // scroll_offset=3: vb=11, vt=8 → msg1 skip 6
            (1, 6),
            // scroll_offset=4: vb=10, vt=7 → msg1 skip 5
            (1, 5),
            // scroll_offset=5: vb=9, vt=6 → msg1 skip 4
            (1, 4),
            // scroll_offset=6: vb=8, vt=5 → msg1 skip 3
            (1, 3),
            // scroll_offset=7: vb=7, vt=4 → msg1 skip 2
            (1, 2),
            // scroll_offset=8: vb=6, vt=3 → msg1 skip 1
            (1, 1),
            // scroll_offset=9: vb=5, vt=2 → msg1 skip 0
            (1, 0),
            // scroll_offset=10: vb=4, vt=1 → msg0 skip 1
            (0, 1),
            // scroll_offset=11: vb=3, vt=0 → msg0 skip 0
            (0, 0),
        ];

        let total_lines: usize = heights.iter().sum();
        for (offset, &(exp_msg, exp_skip)) in expected.iter().enumerate() {
            let view_bottom = total_lines.saturating_sub(offset);
            let view_top = view_bottom.saturating_sub(viewport);
            let result = find_view_start(&heights, view_top);
            assert_eq!(
                result,
                (exp_msg, exp_skip),
                "scroll_offset={offset}, view_top={view_top}"
            );
        }
    }
}
